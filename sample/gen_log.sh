#!/bin/bash
# 演示用日志追加器:每 0.5s 向 sample/app.log 追加一行
# 用法: ./sample/gen_log.sh [文件] [间隔秒]
FILE="${1:-sample/app.log}"
INTERVAL="${2:-0.5}"
i=0
while true; do
  i=$((i + 1))
  case $((i % 10)) in
    0) LEVEL="ERROR"; MSG="upstream timeout url=https://api.example.com/v1/health retry=$i" ;;
    3|7) LEVEL="WARN"; MSG="queue depth $((9000 + i)) > threshold 8000" ;;
    5) LEVEL="DEBUG"; MSG="gc pause 0.$((RANDOM % 9))ms heap=512MB" ;;
    *) LEVEL="INFO"; MSG="request id=c$(printf '%03d' $i) method=GET path=/v1/items/$i status=200 dur=$((RANDOM % 50))ms" ;;
  esac
  echo "$(date -u +%Y-%m-%dT%H:%M:%S.000Z) $LEVEL  $MSG" >> "$FILE"
  sleep "$INTERVAL"
done
