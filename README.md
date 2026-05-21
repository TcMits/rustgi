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
    rustgi.RustgiConfig().set_address("0.0.0.0:8000").set_max_body_size(1024),
)
```

### Benchmarks

rustgi
```sh
└❯ wrk -t4 -c50 -d30s --latency http://0.0.0.0:8000
Running 30s test @ http://0.0.0.0:8000
  4 threads and 50 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency   304.21us   73.87us   2.62ms   67.57%
    Req/Sec    38.51k     1.43k   42.18k    74.58%
  Latency Distribution
     50%  292.00us
     75%  335.00us
     90%  426.00us
     99%  462.00us
  4614102 requests in 30.10s, 453.24MB read
Requests/sec: 153292.39
Transfer/sec:     15.06MB
```
