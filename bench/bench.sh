#!/bin/sh

echo "----------------------------"
echo "Benchmark on rpxy"
ab -c 100 -n 10000 http://127.0.0.1:8080/index.html # TODO: localhost = 127.0.0.1を解決できるように決めておかんとだめそう

echo "----------------------------"
echo "Benchmark on nginx"
ab -c 100 -n 10000  http://127.0.0.1:8090/index.html

echo "----------------------------"
echo "Benchmark on caddy"
ab -c 100 -n 10000  http://127.0.0.1:8100/index.html
