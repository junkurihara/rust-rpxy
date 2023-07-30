#!/bin/sh

# echo "----------------------------"
# echo "Benchmark on rpxy"
# ab -c 100 -n 10000 http://127.0.0.1:8080/index.html

# echo "----------------------------"
# echo "Benchmark on nginx"
# ab -c 100 -n 10000  http://127.0.0.1:8090/index.html

# echo "----------------------------"
# echo "Benchmark on caddy"
# ab -c 100 -n 10000  http://127.0.0.1:8100/index.html

echo "----------------------------"
echo "Benchmark [Any Arch]"

echo "----------------------------"
echo "Benchmark on rpxy"
#wrk -t8 -c100 -d30s http://127.0.0.1:8080/index.html
rewrk -c 512 -t 4 -d 15s -h http://localhost:8080 --pct

echo "sleep 3 secs"
sleep 3

echo "----------------------------"
echo "Benchmark on nginx"
# wrk -t8 -c100 -d30s http://127.0.0.1:8090/index.html
rewrk -c 512 -t 4 -d 15s -h http://localhost:8090 --pct

echo "sleep 3 secs"
sleep 3

echo "----------------------------"
echo "Benchmark on caddy"
# wrk -t8 -c100 -d30s http://127.0.0.1:8100/index.html
rewrk -c 512 -t 4 -d 15s -h http://localhost:8100 --pct
