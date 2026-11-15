#!/usr/bin/env bash
# Prints Valkey commands for demo data; pipe into valkey-cli / redis-cli:
#   scripts/seed/valkey-seed.sh | docker compose -f docker-compose.dev.yml exec -T valkey valkey-cli
set -euo pipefail
for i in $(seq 1 1500); do
  echo "HSET user:$i:session user_id $i email customer$i@example.com plan $([ $((i % 5)) -eq 0 ] && echo pro || echo free) last_seen 2026-10-31T10:00:00Z"
  echo "EXPIRE user:$i:session $((3600 + i))"
done
for i in $(seq 1 300); do
  echo "RPUSH cart:$i SKU-$(printf %04d $((i % 200 + 1))) SKU-$(printf %04d $((i * 7 % 200 + 1)))"
done
for i in $(seq 1 100); do echo "ZADD leaderboard:weekly $((i * 37 % 1000)) player:$i"; done
for i in $(seq 1 50); do echo "SADD feature:beta-users user:$((i * 13))"; done
echo 'SET config:app "{\"theme\":\"dark\",\"retries\":3,\"regions\":[\"af-south-1\",\"eu-west-1\"]}"'
echo "SET counter:page-views 48213"
echo "XADD events:orders * order_id 1 status paid"
echo "XADD events:orders * order_id 2 status shipped"
echo "SELECT 1"
for i in $(seq 1 20); do echo "SET rate:ip:10.0.0.$i $i EX 600"; done
