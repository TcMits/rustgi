# rustgi

rustgi is a fast, lightweight Web Server Gateway Interface (WSGI) that allows you to run Python web applications using Rust. It provides a high-performance and efficient environment for hosting WSGI-compatible applications.

## Getting Started

### Prerequisites

Before running rustgi, make sure you have Rust and Cargo installed on your system. You can install them by following the [official Rust installation guide](https://www.rust-lang.org/tools/install)

### Installation

You can install rustgi with pip

```sh
pip install git+https://github.com/TcMits/rustgi.git
```

## Usage

```python
import rustgi
from app import application

rustgi.serve(
    application,
    rustgi.RustgiConfig().set_address("0.0.0.0:8000").set_worker_threads(2),
)
```

## Performance

rustgi aims to provide high performance and efficient request handling. While it is still under development and based on the [hyper rc3](https://github.com/hyperium/hyper), it may not be as stable as mature WSGI servers. However, it strives to offer comparable performance to servers like [bjoern](https://github.com/jonashaag/bjoern).

### Benchmarks

bjoern
```sh
└❯ wrk -t4 -c1000 -d30s --latency http://0.0.0.0:8000
Running 30s test @ http://0.0.0.0:8000
  4 threads and 1000 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    12.34ms   14.42ms 642.48ms   99.45%
    Req/Sec    21.35k     2.17k   23.36k    96.50%
  Latency Distribution
     50%   11.52ms
     75%   11.95ms
     90%   12.41ms
     99%   15.20ms
  2549975 requests in 30.03s, 218.87MB read
  Socket errors: connect 0, read 5950, write 0, timeout 0
Requests/sec:  84922.45
Transfer/sec:      7.29MB
```

rustgi (single thread)
```sh
└❯ wrk -t4 -c1000 -d30s --latency http://0.0.0.0:8000
Running 30s test @ http://0.0.0.0:8000
  4 threads and 1000 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    19.03ms    2.17ms  88.34ms   97.13%
    Req/Sec    13.15k     1.06k   14.52k    97.08%
  Latency Distribution
     50%   18.73ms
     75%   19.16ms
     90%   19.90ms
     99%   22.57ms
  1570181 requests in 30.01s, 154.24MB read
  Socket errors: connect 0, read 3369, write 0, timeout 0
Requests/sec:  52315.06
Transfer/sec:      5.14MB
```

rustgi (2 worker threads)
```sh
└❯ wrk -t4 -c1000 -d30s --latency http://0.0.0.0:8000
Running 30s test @ http://0.0.0.0:8000
  4 threads and 1000 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency    24.01ms   31.44ms 434.07ms   86.64%
    Req/Sec    18.06k     2.04k   22.17k    88.42%
  Latency Distribution
     50%   15.53ms
     75%   35.27ms
     90%   64.89ms
     99%  139.58ms
  2156001 requests in 30.02s, 211.78MB read
  Socket errors: connect 0, read 3480, write 0, timeout 0
Requests/sec:  71808.98
Transfer/sec:      7.05MB
```
