import json
import logging
import rustgi
import threading

FORMAT = "%(levelname)s %(name)s %(asctime)-15s %(filename)s:%(lineno)d %(message)s"
logging.basicConfig(format=FORMAT)
logger = logging.getLogger()
logger.setLevel(logging.DEBUG)
_local_state = threading.local()


def info(environ, protocol):
    protocol("200 OK", [("content-type", "application/json")])
    return [
        json.dumps(
            {
                "scheme": environ["wsgi.url_scheme"],
                "method": environ["REQUEST_METHOD"],
                "path": environ["PATH_INFO"],
                "query_string": environ["QUERY_STRING"],
                "content_length": int(environ["CONTENT_LENGTH"]),
                "headers": {k: v for k, v in environ.items() if k.startswith("HTTP_")},
            }
        ).encode("utf8")
    ]


def echo(environ, protocol):
    response = environ["wsgi.input"].read()
    protocol(
        "200 OK",
        [
            ("content-type", "text/plain; charset=utf-8"),
            ("content-length", str(len(response))),
        ],
    )

    return [response]


def echo_iter(environ, protocol):
    protocol("200 OK", [("content-type", "text/plain; charset=utf-8")])
    for line in environ["wsgi.input"]:
        yield line


def err_app(environ, protocol):
    1 / 0


def empty_app(environ, protocol):
    protocol("204 OK", [("content-type", "text/plain; charset=utf-8")])

    return b""


def increment(environ, protocol):
    value = _local_state.__dict__.get("value", 0)
    _local_state.__dict__["value"] = value + 1

    protocol(
        "200 OK",
        [
            ("content-type", "text/plain; charset=utf-8"),
            ("content-length", str(len(str(value)))),
        ],
    )
    return [str(value).encode("utf8")]


def app(environ, protocol):
    return {
        "/info": info,
        "/echo": echo,
        "/echo_iter": echo_iter,
        "/err_app": err_app,
        "/empty_app": empty_app,
        "/mã": echo,
        "/": echo,
        "/increment": increment,
        "": echo,
    }[environ["PATH_INFO"].encode("iso-8859-1").decode()](environ, protocol)


rustgi.serve(
    app,
    rustgi.RustgiConfig().set_address("0.0.0.0:8000").set_max_body_size(1024 * 66 * 7),
)
