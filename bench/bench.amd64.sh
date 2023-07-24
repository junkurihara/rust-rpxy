#!/bin/sh

echo "----------------------------"
echo "Benchmark [x86_64] with ReWrk"

echo "----------------------------"
echo "Benchmark on rpxy"
rewrk -c 512 -t 4 -d 15s -h http://localhost:8080 --pct

echo "sleep 3 secs"
sleep 3

echo "----------------------------"
echo "Benchmark on nginx"
rewrk -c 512 -t 4 -d 15s -h http://localhost:8090 --pct

echo "sleep 3 secs"
sleep 3

echo "----------------------------"
echo "Benchmark on caddy"
rewrk -c 512 -t 4 -d 15s -h http://localhost:8100 --pct

echo "sleep 3 secs"
sleep 3

echo "----------------------------"
echo "Benchmark [x86_64] with Wrk"

echo "----------------------------"
echo "Benchmark on rpxy"
wrk -c 512 -t 4 -d 15s http://localhost:8080

echo "sleep 3 secs"
sleep 3

echo "----------------------------"
echo "Benchmark on nginx"
wrk -c 512 -t 4 -d 15s http://localhost:8090

echo "sleep 3 secs"
sleep 3

echo "----------------------------"
echo "Benchmark on caddy"
wrk -c 512 -t 4 -d 15s http://localhost:8100

echo "sleep 3 secs"
sleep 3

echo "----------------------------"
echo "Benchmark on sozu"
wrk -c 512 -t 4 -d 15s http://localhost:8110
