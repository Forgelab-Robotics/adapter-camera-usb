#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "错误：USB Camera backend 仅支持 Linux。" >&2
  exit 1
fi

if [[ "${EUID}" -ne 0 ]]; then
  echo "请使用 sudo 运行：sudo scripts/install_permissions.sh" >&2
  exit 1
fi

target_user="${SUDO_USER:-}"
if [[ -z "${target_user}" || "${target_user}" == "root" ]]; then
  echo "无法确定普通用户；请通过 sudo 从目标用户运行。" >&2
  exit 1
fi

if ! getent group video >/dev/null; then
  groupadd --system video
fi

usermod --append --groups video "${target_user}"

echo "已将 ${target_user} 加入 video 组。"
echo "未写入或覆盖系统 udev 规则；设备节点权限继续由发行版/厂商规则管理。"
echo "请重新登录后运行：cargo run --bin usb_camera -- check-environment"
