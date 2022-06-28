#!/bin/sh

echo "----------------------------"
echo "Benchmark on rpxy"
ab -c 32 -n 10000 http://127.0.0.1:8080/ # TODO: localhost = 127.0.0.1を解決できるように決めておかんとだめそう

echo "----------------------------"
echo "Benchmark on nginx"
ab -c 32 -n 10000 http://127.0.0.1:8090/

echo "----------------------------"
echo "Benchmark on caddy"
ab -c 32 -n 10000 http://127.0.0.1:8100/
