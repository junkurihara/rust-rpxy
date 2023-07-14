# Sample Benchmark Result

Done at Jul. 15, 2023

This test simply measures the performance of several reverse proxy through HTTP/1.1 by the following command using [`rewrk`](https://github.com/lnx-search/rewrk).

```bash
$ rewrk -c 512 -t 4 -d 15s -h http://localhost:8080 --pct
```

## Environment

- `rpxy` commit id: `1da7e5bfb77d1ce4ee8d6cfc59b1c725556fc192`
- Docker Desktop 4.21.1 (114176)
- ReWrk 0.3.1
- Macbook Pro '14 (2021, M1 Max, 64GB RAM)



## Result

```
----------------------------
Benchmark on rpxy
Beginning round 1...
Benchmarking 512 connections @ http://localhost:8080 for 15 second(s)
  Latencies:
    Avg      Stdev    Min      Max
    19.64ms  8.85ms   0.67ms   113.22ms
  Requests:
    Total: 390078  Req/Sec: 26011.25
  Transfer:
    Total: 304.85 MB Transfer Rate: 20.33 MB/Sec
+ --------------- + --------------- +
|   Percentile    |   Avg Latency   |
+ --------------- + --------------- +
|      99.9%      |     79.24ms     |
|       99%       |     54.28ms     |
|       95%       |     42.50ms     |
|       90%       |     37.82ms     |
|       75%       |     31.54ms     |
|       50%       |     26.37ms     |
+ --------------- + --------------- +

721 Errors: error shutting down connection: Socket is not connected (os error 57)

sleep 3 secs
----------------------------
Benchmark on nginx
Beginning round 1...
Benchmarking 512 connections @ http://localhost:8090 for 15 second(s)
  Latencies:
    Avg      Stdev    Min      Max
    33.26ms  15.18ms  1.40ms   118.94ms
  Requests:
    Total: 230268  Req/Sec: 15356.08
  Transfer:
    Total: 186.77 MB Transfer Rate: 12.46 MB/Sec
+ --------------- + --------------- +
|   Percentile    |   Avg Latency   |
+ --------------- + --------------- +
|      99.9%      |     99.91ms     |
|       99%       |     83.74ms     |
|       95%       |     70.67ms     |
|       90%       |     64.03ms     |
|       75%       |     54.32ms     |
|       50%       |     45.19ms     |
+ --------------- + --------------- +

677 Errors: error shutting down connection: Socket is not connected (os error 57)

sleep 3 secs
----------------------------
Benchmark on caddy
Beginning round 1...
Benchmarking 512 connections @ http://localhost:8100 for 15 second(s)
  Latencies:
    Avg      Stdev    Min      Max
    48.51ms  50.74ms  0.34ms   554.58ms
  Requests:
    Total: 157239  Req/Sec: 10485.98
  Transfer:
    Total: 125.99 MB Transfer Rate: 8.40 MB/Sec
+ --------------- + --------------- +
|   Percentile    |   Avg Latency   |
+ --------------- + --------------- +
|      99.9%      |    473.82ms     |
|       99%       |    307.16ms     |
|       95%       |    212.28ms     |
|       90%       |    169.05ms     |
|       75%       |    115.92ms     |
|       50%       |     80.24ms     |
+ --------------- + --------------- +

708 Errors: error shutting down connection: Socket is not connected (os error 57)
```
