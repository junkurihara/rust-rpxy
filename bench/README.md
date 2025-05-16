# Sample Benchmark Results

This test simply measures the performance of several reverse proxy through HTTP/1.1 by the following command using [`rewrk`](https://github.com/lnx-search/rewrk).

```sh:
$ rewrk -c 512 -t 4 -d 15s -h http://localhost:8080 --pct
```

## Tests on `linux/arm64/v8`

Done at May. 17, 2025

### Environment

- `rpxy` commit id: `e259e0b58897258d98fdb7504a1cbcbd7c5b37db`
- Docker Desktop 4.41.2 (191736)
- ReWrk 0.3.2
- Mac mini (2024, M4 Pro, 64GB RAM)

The docker images of `nginx` and `caddy` for `linux/arm64/v8` are pulled from the official registry.

### Result for `rpxy`, `nginx` and `caddy`

```bash
Benchmark on rpxy
Beginning round 1...
Benchmarking 512 connections @ http://localhost:8080 for 15 second(s)
  Latencies:
    Avg      Stdev    Min      Max
    6.90ms   3.42ms   0.78ms   80.26ms
  Requests:
    Total: 1107885 Req/Sec: 73866.03
  Transfer:
    Total: 867.44 MB Transfer Rate: 57.83 MB/Sec
+ --------------- + --------------- +
|   Percentile    |   Avg Latency   |
+ --------------- + --------------- +
|      99.9%      |     49.76ms     |
|       99%       |     29.57ms     |
|       95%       |     15.78ms     |
|       90%       |     13.05ms     |
|       75%       |     10.41ms     |
|       50%       |     8.72ms      |
+ --------------- + --------------- +

sleep 3 secs
----------------------------
Benchmark on nginx
Beginning round 1...
Benchmarking 512 connections @ http://localhost:8090 for 15 second(s)
  Latencies:
    Avg      Stdev    Min      Max
    11.65ms  14.04ms  0.40ms   205.93ms
  Requests:
    Total: 654978  Req/Sec: 43666.56
  Transfer:
    Total: 532.81 MB Transfer Rate: 35.52 MB/Sec
+ --------------- + --------------- +
|   Percentile    |   Avg Latency   |
+ --------------- + --------------- +
|      99.9%      |    151.00ms     |
|       99%       |    102.80ms     |
|       95%       |     62.44ms     |
|       90%       |     42.98ms     |
|       75%       |     26.44ms     |
|       50%       |     18.25ms     |
+ --------------- + --------------- +

512 Errors: connection closed

sleep 3 secs
----------------------------
Benchmark on caddy
Beginning round 1...
Benchmarking 512 connections @ http://localhost:8100 for 15 second(s)
  Latencies:
    Avg      Stdev    Min      Max
    77.54ms  368.11ms  0.37ms   6770.73ms
  Requests:
    Total:  86963  Req/Sec: 5798.35
  Transfer:
    Total: 70.00 MB Transfer Rate: 4.67 MB/Sec
+ --------------- + --------------- +
|   Percentile    |   Avg Latency   |
+ --------------- + --------------- +
|      99.9%      |    5789.65ms    |
|       99%       |    3407.02ms    |
|       95%       |    1022.31ms    |
|       90%       |    608.17ms     |
|       75%       |    281.95ms     |
|       50%       |    149.29ms     |
+ --------------- + --------------- +
```

## Results on `linux/amd64`

Done at Jul. 24, 2023

### Environment

- `rpxy` commit id: `7c0945a5124418aa9a1024568c1989bb77cf312f`
- Docker Desktop 4.21.1 (114176)
- ReWrk 0.3.2 and Wrk 0.4.2
- iMac '27 (2020, 10-Core Intel Core i9, 128GB RAM)

The docker images of `nginx` and `caddy` for `linux/amd64` were pulled from the official registry. For `Sozu`, the official docker image from its developers was still version 0.11.0 (currently the latest version is 0.15.2). So we built it by ourselves locally using the `Sozu`'s official [`Dockerfile`](https://github.com/sozu-proxy/sozu/blob/main/Dockerfile).

Also, when `Sozu` is configured as an HTTP reverse proxy, it cannot handle HTTP request messages emit from `ReWrk` due to hostname parsing errors though it can correctly handle messages dispatched from `curl` and browsers. So, we additionally test using [`Wrk`](https://github.com/wg/wrk) to examine `Sozu` with the following command.

```sh:
$ wrk -c 512 -t 4 -d 15s http://localhost:8110
```

<!-- ```
ERROR  Error connecting to backend: Could not get cluster id from request: Host not found: http://localhost:8110/: Hostname parsing failed for host http://localhost:8110/: Parsing Error: Error { input: [58, 47, 47, 108, 111, 99, 97, 108, 104, 111, 115, 116, 58, 56, 49, 49, 48, 47], code: Eof }
``` -->

### Result

#### With ReWrk for `rpxy`, `nginx` and `caddy`

```
----------------------------
Benchmark [x86_64] with ReWrk
----------------------------
Benchmark on rpxy
Beginning round 1...
Benchmarking 512 connections @ http://localhost:8080 for 15 second(s)
  Latencies:
    Avg      Stdev    Min      Max
    20.37ms  8.95ms   1.63ms   160.27ms
  Requests:
    Total: 376345  Req/Sec: 25095.19
  Transfer:
    Total: 295.61 MB Transfer Rate: 19.71 MB/Sec
+ --------------- + --------------- +
|   Percentile    |   Avg Latency   |
+ --------------- + --------------- +
|      99.9%      |    112.50ms     |
|       99%       |     61.33ms     |
|       95%       |     44.26ms     |
|       90%       |     38.74ms     |
|       75%       |     32.00ms     |
|       50%       |     26.82ms     |
+ --------------- + --------------- +

626 Errors: error shutting down connection: Socket is not connected (os error 57)

sleep 3 secs
----------------------------
Benchmark on nginx
Beginning round 1...
Benchmarking 512 connections @ http://localhost:8090 for 15 second(s)
  Latencies:
    Avg      Stdev    Min      Max
    23.45ms  12.42ms  1.18ms   154.44ms
  Requests:
    Total: 326685  Req/Sec: 21784.73
  Transfer:
    Total: 265.22 MB Transfer Rate: 17.69 MB/Sec
+ --------------- + --------------- +
|   Percentile    |   Avg Latency   |
+ --------------- + --------------- +
|      99.9%      |     96.85ms     |
|       99%       |     73.93ms     |
|       95%       |     57.57ms     |
|       90%       |     50.36ms     |
|       75%       |     40.57ms     |
|       50%       |     32.70ms     |
+ --------------- + --------------- +

657 Errors: error shutting down connection: Socket is not connected (os error 57)

sleep 3 secs
----------------------------
Benchmark on caddy
Beginning round 1...
Benchmarking 512 connections @ http://localhost:8100 for 15 second(s)
  Latencies:
    Avg      Stdev    Min      Max
    45.71ms  50.47ms  0.88ms   908.49ms
  Requests:
    Total: 166917  Req/Sec: 11129.80
  Transfer:
    Total: 133.77 MB Transfer Rate: 8.92 MB/Sec
+ --------------- + --------------- +
|   Percentile    |   Avg Latency   |
+ --------------- + --------------- +
|      99.9%      |    608.92ms     |
|       99%       |    351.18ms     |
|       95%       |    210.56ms     |
|       90%       |    162.68ms     |
|       75%       |    106.97ms     |
|       50%       |     73.90ms     |
+ --------------- + --------------- +

646 Errors: error shutting down connection: Socket is not connected (os error 57)

sleep 3 secs
```

#### With Wrk for `rpxy`, `nginx`, `caddy` and `sozu`

```
----------------------------
Benchmark [x86_64] with Wrk
----------------------------
Benchmark on rpxy
Running 15s test @ http://localhost:8080
  4 threads and 512 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    18.68ms    8.09ms 122.64ms   74.03%
    Req/Sec     6.95k   815.23     8.45k    83.83%
  414819 requests in 15.01s, 326.37MB read
  Socket errors: connect 0, read 608, write 0, timeout 0
Requests/sec:  27627.79
Transfer/sec:     21.74MB

sleep 3 secs
----------------------------
Benchmark on nginx
Running 15s test @ http://localhost:8090
  4 threads and 512 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    23.34ms   13.80ms 126.06ms   74.66%
    Req/Sec     5.71k   607.41     7.07k    73.17%
  341127 requests in 15.03s, 277.50MB read
  Socket errors: connect 0, read 641, write 0, timeout 0
Requests/sec:  22701.54
Transfer/sec:     18.47MB

sleep 3 secs
----------------------------
Benchmark on caddy
Running 15s test @ http://localhost:8100
  4 threads and 512 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    54.19ms   55.63ms 674.53ms   88.55%
    Req/Sec     2.92k     1.40k    5.57k    56.17%
  174748 requests in 15.03s, 140.61MB read
  Socket errors: connect 0, read 660, write 0, timeout 0
  Non-2xx or 3xx responses: 70
Requests/sec:  11624.63
Transfer/sec:      9.35MB

sleep 3 secs
----------------------------
Benchmark on sozu
Running 15s test @ http://localhost:8110
  4 threads and 512 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    19.78ms    4.89ms  98.09ms   76.88%
    Req/Sec     6.49k   824.75     8.11k    76.17%
  387744 requests in 15.02s, 329.11MB read
  Socket errors: connect 0, read 647, write 0, timeout 0
Requests/sec:  25821.93
Transfer/sec:     21.92MB
```
