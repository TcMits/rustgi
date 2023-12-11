# rustgi

rustgi is a fast, lightweight, single-thread Web Server Gateway Interface (WSGI) that allows you to run Python web applications using Rust. It provides a high-performance and efficient environment for hosting WSGI-compatible applications.

## Features

- serve WSGI
- set max body size

## Getting Started

### Installation

You can install rustgi with pip

```sh
pip install rustgi
```

## Usage

```python
import rustgi
from app import application

rustgi.serve(
    application,
    rustgi.RustgiConfig().set_address("0.0.0.0:8000"),
)
```

### Benchmarks

rustgi
```sh
└❯ wrk -t4 -c1000 -d30s --latency http://0.0.0.0:8000
Running 30s test @ http://0.0.0.0:8000
  4 threads and 1000 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency     5.03ms  353.39us  10.73ms   87.20%
    Req/Sec    12.42k     1.90k   19.36k    53.62%
  Latency Distribution
     50%    4.97ms
     75%    5.14ms
     90%    5.35ms
     99%    6.53ms
  1487223 requests in 30.10s, 127.65MB read
  Socket errors: connect 751, read 81, write 0, timeout 0
Requests/sec:  49403.93
Transfer/sec:      4.24MB
```
