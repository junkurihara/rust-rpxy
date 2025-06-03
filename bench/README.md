# Sample Benchmark Results

This test simply measures the performance of several reverse proxy through HTTP/1.1 by the following command using [`rewrk`](https://github.com/lnx-search/rewrk).

```sh:
rewrk -c 512 -t 4 -d 15s -h http://localhost:8080 --pct
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

Done at May 20, 2025

### Environment

- `rpxy` commit id: `e259e0b58897258d98fdb7504a1cbcbd7c5b37db`
- Docker Desktop 4.41.2 (192736)
- ReWrk 0.3.2 and Wrk 0.4.2
- iMac '27 (2020, 10-Core Intel Core i9, 128GB RAM)

The docker images of `nginx` and `caddy` for `linux/amd64` were pulled from the official registry. For `Sozu`, the official docker image from its developers was still version 0.11.0 (currently the latest version is 0.15.2). So we built it by ourselves locally using the `Sozu`'s official [`Dockerfile`](https://github.com/sozu-proxy/sozu/blob/main/Dockerfile).

Also, when `Sozu` is configured as an HTTP reverse proxy, it cannot handle HTTP request messages emit from `ReWrk` due to hostname parsing errors though it can correctly handle messages dispatched from `curl` and browsers. So, we additionally test using [`Wrk`](https://github.com/wg/wrk) to examine `Sozu` with the following command.

```bash
wrk -c 512 -t 4 -d 15s http://localhost:8110
```

<!-- ```
ERROR  Error connecting to backend: Could not get cluster id from request: Host not found: http://localhost:8110/: Hostname parsing failed for host http://localhost:8110/: Parsing Error: Error { input: [58, 47, 47, 108, 111, 99, 97, 108, 104, 111, 115, 116, 58, 56, 49, 49, 48, 47], code: Eof }
``` -->

### Result

#### With ReWrk for `rpxy`, `nginx` and `caddy`

```bash
----------------------------
Benchmark [x86_64] with ReWrk
----------------------------
Benchmark on rpxy
Beginning round 1...
Benchmarking 512 connections @ http://localhost:8080 for 15 second(s)
  Latencies:
    Avg      Stdev    Min      Max
    15.75ms  6.75ms   1.75ms   124.25ms
  Requests:
    Total: 486635  Req/Sec: 32445.33
  Transfer:
    Total: 381.02 MB Transfer Rate: 25.40 MB/Sec
+ --------------- + --------------- +
|   Percentile    |   Avg Latency   |
+ --------------- + --------------- +
|      99.9%      |     91.91ms     |
|       99%       |     55.53ms     |
|       95%       |     34.87ms     |
|       90%       |     29.55ms     |
|       75%       |     23.99ms     |
|       50%       |     20.17ms     |
+ --------------- + --------------- +

sleep 3 secs
----------------------------
Benchmark on nginx
Beginning round 1...
Benchmarking 512 connections @ http://localhost:8090 for 15 second(s)
  Latencies:
    Avg      Stdev    Min      Max
    24.02ms  15.84ms  1.31ms   207.97ms
  Requests:
    Total: 318516  Req/Sec: 21236.67
  Transfer:
    Total: 259.11 MB Transfer Rate: 17.28 MB/Sec
+ --------------- + --------------- +
|   Percentile    |   Avg Latency   |
+ --------------- + --------------- +
|      99.9%      |    135.56ms     |
|       99%       |     92.59ms     |
|       95%       |     68.54ms     |
|       90%       |     58.75ms     |
|       75%       |     45.88ms     |
|       50%       |     35.64ms     |
+ --------------- + --------------- +

sleep 3 secs
----------------------------
Benchmark on caddy
Beginning round 1...
Benchmarking 512 connections @ http://localhost:8100 for 15 second(s)
  Latencies:
    Avg      Stdev    Min      Max
    74.60ms  181.26ms  0.94ms   2723.20ms
  Requests:
    Total: 101893  Req/Sec: 6792.16
  Transfer:
    Total: 82.03 MB Transfer Rate: 5.47 MB/Sec
+ --------------- + --------------- +
|   Percentile    |   Avg Latency   |
+ --------------- + --------------- +
|      99.9%      |    2232.12ms    |
|       99%       |    1517.73ms    |
|       95%       |    624.63ms     |
|       90%       |    406.69ms     |
|       75%       |    222.42ms     |
|       50%       |    133.46ms     |
+ --------------- + --------------- +
```

#### With Wrk for `rpxy`, `nginx`, `caddy` and `sozu`

```bash
----------------------------
Benchmark [x86_64] with Wrk
----------------------------
Benchmark on rpxy
Running 15s test @ http://localhost:8080
  4 threads and 512 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    15.65ms    6.94ms 104.73ms   81.28%
    Req/Sec     8.36k     0.90k    9.90k    77.83%
  499550 requests in 15.02s, 391.14MB read
Requests/sec:  33267.61
Transfer/sec:     26.05MB

sleep 3 secs
----------------------------
Benchmark on nginx
Running 15s test @ http://localhost:8090
  4 threads and 512 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    24.26ms   15.29ms 167.43ms   73.34%
    Req/Sec     5.53k   493.14     6.91k    69.67%
  330569 requests in 15.02s, 268.91MB read
Requests/sec:  22014.96
Transfer/sec:     17.91MB

sleep 3 secs
----------------------------
Benchmark on caddy
Running 15s test @ http://localhost:8100
  4 threads and 512 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency   212.89ms  300.23ms   1.99s    86.56%
    Req/Sec     1.31k     1.64k    5.72k    78.79%
  67749 requests in 15.04s, 51.97MB read
  Socket errors: connect 0, read 0, write 0, timeout 222
  Non-2xx or 3xx responses: 3686
Requests/sec:   4505.12
Transfer/sec:      3.46MB

sleep 3 secs
----------------------------
Benchmark on sozu
Running 15s test @ http://localhost:8110
  4 threads and 512 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    34.68ms    6.30ms  90.21ms   72.49%
    Req/Sec     3.69k   397.85     5.08k    73.00%
  220655 requests in 15.01s, 187.29MB read
Requests/sec:  14699.17
Transfer/sec:     12.48MB
```
