# Sample Benchmark Result

Using `rewrk` and Docker on a Macbook Pro 14 to simply measure the performance of several reverse proxy through HTTP1.1.

```
$ rewrk -c 512 -t 4 -d 15s -h http://localhost:8080 --pct
```

```
----------------------------
Benchmark on rpxy
Beginning round 1...
Benchmarking 512 connections @ http://localhost:8080 for 15 second(s)
  Latencies:
    Avg      Stdev    Min      Max
    26.81ms  11.96ms  2.96ms   226.04ms
  Requests:
    Total: 285390  Req/Sec: 19032.01
  Transfer:
    Total: 222.85 MB Transfer Rate: 14.86 MB/Sec
+ --------------- + --------------- +
|   Percentile    |   Avg Latency   |
+ --------------- + --------------- +
|      99.9%      |    145.89ms     |
|       99%       |     81.33ms     |
|       95%       |     59.08ms     |
|       90%       |     51.67ms     |
|       75%       |     42.45ms     |
|       50%       |     35.39ms     |
+ --------------- + --------------- +

767 Errors: error shutting down connection: Socket is not connected (os error 57)

sleep 3 secs
----------------------------
Benchmark on nginx
Beginning round 1...
Benchmarking 512 connections @ http://localhost:8090 for 15 second(s)
  Latencies:
    Avg      Stdev    Min      Max
    38.39ms  21.06ms  2.91ms   248.32ms
  Requests:
    Total: 199210  Req/Sec: 13288.91
  Transfer:
    Total: 161.46 MB Transfer Rate: 10.77 MB/Sec
+ --------------- + --------------- +
|   Percentile    |   Avg Latency   |
+ --------------- + --------------- +
|      99.9%      |    164.33ms     |
|       99%       |    121.55ms     |
|       95%       |     96.43ms     |
|       90%       |     85.05ms     |
|       75%       |     67.80ms     |
|       50%       |     53.85ms     |
+ --------------- + --------------- +

736 Errors: error shutting down connection: Socket is not connected (os error 57)

sleep 3 secs
----------------------------
Benchmark on caddy
Beginning round 1...
Benchmarking 512 connections @ http://localhost:8100 for 15 second(s)
  Latencies:
    Avg      Stdev    Min      Max
    83.17ms  73.71ms  1.24ms   734.67ms
  Requests:
    Total:  91685  Req/Sec: 6114.05
  Transfer:
    Total: 73.20 MB Transfer Rate: 4.88 MB/Sec
+ --------------- + --------------- +
|   Percentile    |   Avg Latency   |
+ --------------- + --------------- +
|      99.9%      |    642.29ms     |
|       99%       |    507.21ms     |
|       95%       |    324.34ms     |
|       90%       |    249.55ms     |
|       75%       |    174.62ms     |
|       50%       |    128.85ms     |
+ --------------- + --------------- +

740 Errors: error shutting down connection: Socket is not connected (os error 57)
```
