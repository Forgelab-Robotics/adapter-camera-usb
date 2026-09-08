use std::ffi::OsStr;

use dora_node_api::{MetadataParameters, Parameter};

use forge_common::observability::{
    Clock, MetadataContainer, MetadataValueRef, OBSERVATION_KEYS, OBSERVATION_VERSION,
    ORIGIN_ID_KEY, ORIGIN_TIME_KEY, Observer, PUBLISH_TIME_KEY, PublicationFields, PublicationMode,
    VERSION_KEY, parse_metadata,
};

pub fn observer_from_env() -> Option<Observer> {
    enabled(std::env::var_os("FORGE_OBSERVABILITY").as_deref()).then(Observer::default)
}

fn enabled(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new("1"))
}

fn parameter_to_metadata_ref(value: Option<&Parameter>) -> Option<MetadataValueRef<'_>> {
    value.map(|value| match value {
        Parameter::Integer(value) => MetadataValueRef::Integer(*value),
        Parameter::String(value) => MetadataValueRef::String(value),
        _ => MetadataValueRef::Other,
    })
}

fn apply_publication_fields(parameters: &mut MetadataParameters, fields: &PublicationFields) {
    if matches!(fields, PublicationFields::PreserveOpaque) {
        return;
    }
    for key in OBSERVATION_KEYS {
        parameters.remove(key);
    }
    if let PublicationFields::V1(fields) = fields {
        parameters.insert(VERSION_KEY.into(), Parameter::Integer(OBSERVATION_VERSION));
        parameters.insert(
            PUBLISH_TIME_KEY.into(),
            Parameter::Integer(fields.publish_time_ns()),
        );
        if let Some(origin_time_ns) = fields.origin_time_ns() {
            parameters.insert(ORIGIN_TIME_KEY.into(), Parameter::Integer(origin_time_ns));
        }
        if let Some(origin_id) = fields.origin_id() {
            parameters.insert(ORIGIN_ID_KEY.into(), Parameter::String(origin_id.into()));
        }
    }
}

/// Call only after the complete payload is ready; the closure owns the single transport send.
pub fn publish<C: Clock, T, E>(
    observer: Option<&Observer<C>>,
    output_id: &str,
    mut parameters: MetadataParameters,
    send: impl FnOnce(MetadataParameters) -> Result<T, E>,
) -> Result<T, E> {
    let Some(observer) = observer else {
        return send(parameters);
    };

    // A tick schedules capture but is never the image's timing parent, even for unknown versions.
    apply_publication_fields(&mut parameters, &PublicationFields::Clear);
    let outgoing = parse_metadata(MetadataContainer::Mapping, |key| {
        parameter_to_metadata_ref(parameters.get(key))
    });
    let prepared = observer
        .prepare_publication(
            output_id,
            PublicationMode::NewOrigin { origin_id: None },
            Some(&outgoing),
        )
        .expect("cleared camera metadata cannot contain an unknown observability version");
    apply_publication_fields(&mut parameters, prepared.fields());
    let result = send(parameters);
    observer.finish_publication(prepared, result)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use forge_common::metrics::{BoundedMetricsSink, EventReason, MetricsConfig};
    use forge_common::observability::{MetadataIssue, OriginId};

    use super::*;
    use crate::{CAPTURE_TIMESTAMP_KEY, output_parameters};

    struct FixedClock(Option<i64>);

    impl Clock for FixedClock {
        fn unix_time_ns(&self) -> Option<i64> {
            self.0
        }

        fn monotonic_time_ns(&self) -> Option<u64> {
            Some(100)
        }
    }

    fn tick_parameters(version: i64) -> MetadataParameters {
        MetadataParameters::from([
            (VERSION_KEY.into(), Parameter::Integer(version)),
            (PUBLISH_TIME_KEY.into(), Parameter::Integer(10)),
            (ORIGIN_TIME_KEY.into(), Parameter::Integer(5)),
            (
                ORIGIN_ID_KEY.into(),
                Parameter::String("tick-origin".into()),
            ),
            (CAPTURE_TIMESTAMP_KEY.into(), Parameter::Integer(1)),
            ("request_id".into(), Parameter::String("request-1".into())),
        ])
    }

    #[test]
    fn only_exact_one_enables_observability() {
        assert!(!enabled(None));
        for value in ["", "0", "true", "yes", "01", " 1", "1 "] {
            assert!(!enabled(Some(OsStr::new(value))));
        }
        assert!(enabled(Some(OsStr::new("1"))));
    }

    #[test]
    fn typed_parameters_distinguish_missing_and_wrong_types() {
        assert_eq!(parameter_to_metadata_ref(None), None);
        assert_eq!(
            parameter_to_metadata_ref(Some(&Parameter::Integer(123))),
            Some(MetadataValueRef::Integer(123))
        );
        assert_eq!(
            parameter_to_metadata_ref(Some(&Parameter::String("123".into()))),
            Some(MetadataValueRef::String("123"))
        );
        for value in [
            Parameter::Bool(true),
            Parameter::Float(1.0),
            Parameter::ListInt(vec![1]),
            Parameter::ListFloat(vec![1.0]),
            Parameter::ListString(vec!["1".into()]),
        ] {
            assert_eq!(
                parameter_to_metadata_ref(Some(&value)),
                Some(MetadataValueRef::Other)
            );
        }
        for value in [Parameter::String("1".into()), Parameter::Bool(true)] {
            let parameters = MetadataParameters::from([(VERSION_KEY.into(), value)]);
            let parsed = parse_metadata(MetadataContainer::Mapping, |key| {
                parameter_to_metadata_ref(parameters.get(key))
            });
            assert!(parsed.issues().contains(&MetadataIssue::InvalidVersion));
        }
    }

    #[test]
    fn disabled_publication_preserves_existing_metadata_and_sends_once() {
        let parameters = output_parameters(tick_parameters(99), Some(42));
        let expected = parameters.clone();
        let mut sends = 0;
        let result = publish::<FixedClock, _, ()>(None, "image", parameters, |actual| {
            sends += 1;
            assert_eq!(actual, expected);
            Ok(7)
        });
        assert_eq!(result.unwrap(), 7);
        assert_eq!(sends, 1);
    }

    #[test]
    fn camera_clears_stale_and_unknown_tick_origins_before_new_origin() {
        let observer = Observer::with_clock(FixedClock(Some(100)), None);
        for version in [1, 99] {
            let parameters = output_parameters(tick_parameters(version), Some(42));
            let mut sends = 0;
            publish(Some(&observer), "image", parameters, |actual| {
                sends += 1;
                assert_eq!(actual.get(VERSION_KEY), Some(&Parameter::Integer(1)));
                assert_eq!(actual.get(PUBLISH_TIME_KEY), Some(&Parameter::Integer(100)));
                assert_eq!(actual.get(ORIGIN_TIME_KEY), Some(&Parameter::Integer(100)));
                assert!(!actual.contains_key(ORIGIN_ID_KEY));
                assert_eq!(
                    actual.get(CAPTURE_TIMESTAMP_KEY),
                    Some(&Parameter::Integer(42))
                );
                assert_eq!(
                    actual.get("request_id"),
                    Some(&Parameter::String("request-1".into()))
                );
                let parsed = parse_metadata(MetadataContainer::Mapping, |key| {
                    parameter_to_metadata_ref(actual.get(key))
                });
                assert!(parsed.issues().is_empty());
                Ok::<_, ()>(())
            })
            .unwrap();
            assert_eq!(sends, 1);
        }
    }

    #[test]
    fn unavailable_clock_clears_observability_without_blocking_send() {
        let observer = Observer::with_clock(FixedClock(None), None);
        let parameters = output_parameters(tick_parameters(99), None);
        let mut sends = 0;
        publish(Some(&observer), "image", parameters, |actual| {
            sends += 1;
            assert!(
                OBSERVATION_KEYS
                    .iter()
                    .all(|key| !actual.contains_key(*key))
            );
            assert!(!actual.contains_key(CAPTURE_TIMESTAMP_KEY));
            assert_eq!(actual.len(), 1);
            assert!(actual.contains_key("request_id"));
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(sends, 1);
    }

    #[test]
    fn field_adapter_handles_opaque_clear_and_optional_v1_fields() {
        let mut parameters = tick_parameters(99);
        let original = parameters.clone();
        apply_publication_fields(&mut parameters, &PublicationFields::PreserveOpaque);
        assert_eq!(parameters, original);

        apply_publication_fields(&mut parameters, &PublicationFields::Clear);
        assert_eq!(parameters.len(), 2);
        assert!(parameters.contains_key(CAPTURE_TIMESTAMP_KEY));
        assert!(parameters.contains_key("request_id"));

        let observer = Observer::with_clock(FixedClock(Some(100)), None);
        let origin_id = OriginId::new("camera-origin").unwrap();
        let prepared = observer
            .prepare_publication(
                "image",
                PublicationMode::NewOrigin {
                    origin_id: Some(&origin_id),
                },
                None,
            )
            .unwrap();
        apply_publication_fields(&mut parameters, prepared.fields());
        assert_eq!(
            parameters.get(ORIGIN_ID_KEY),
            Some(&Parameter::String("camera-origin".into()))
        );
        observer
            .finish_publication(prepared, Ok::<_, ()>(()))
            .unwrap();

        let prepared = observer
            .prepare_publication("image", PublicationMode::Unlinked, None)
            .unwrap();
        apply_publication_fields(&mut parameters, prepared.fields());
        assert_eq!(parameters.get(VERSION_KEY), Some(&Parameter::Integer(1)));
        assert_eq!(
            parameters.get(PUBLISH_TIME_KEY),
            Some(&Parameter::Integer(100))
        );
        assert!(!parameters.contains_key(ORIGIN_TIME_KEY));
        assert!(!parameters.contains_key(ORIGIN_ID_KEY));
        assert_eq!(
            parameters.get(CAPTURE_TIMESTAMP_KEY),
            Some(&Parameter::Integer(1))
        );
        observer
            .finish_publication(prepared, Ok::<_, ()>(()))
            .unwrap();
    }

    #[test]
    fn publication_finishes_success_and_failure_without_retry_or_error_replacement() {
        let sink = Arc::new(BoundedMetricsSink::new(MetricsConfig::default()).unwrap());
        let observer = Observer::with_clock(FixedClock(Some(100)), Some(Arc::clone(&sink)));
        let mut sends = 0;
        publish(Some(&observer), "image", MetadataParameters::new(), |_| {
            sends += 1;
            Ok::<_, ()>(())
        })
        .unwrap();
        let error = eyre::eyre!("transport failed");
        let original = error.as_ref() as *const (dyn std::error::Error + Send + Sync);
        let returned = publish(Some(&observer), "image", MetadataParameters::new(), |_| {
            sends += 1;
            Err::<(), _>(error)
        })
        .unwrap_err();
        assert!(std::ptr::eq(returned.as_ref(), original));
        assert_eq!(sends, 2);
        let snapshot = sink.snapshot();
        assert!(snapshot.histograms.is_empty());
        assert_eq!(snapshot.counters.len(), 2);
        for reason in [EventReason::Published, EventReason::PublishFailed] {
            let counter = snapshot
                .counters
                .iter()
                .find(|value| value.reason == reason)
                .unwrap();
            assert_eq!(counter.count, 1);
            assert_eq!(counter.output_id.as_ref(), "image");
            assert_eq!(counter.input_id.as_ref(), "");
        }
    }
}
