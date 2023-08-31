import json
import logging
import rustgi
import os


FORMAT = '%(levelname)s %(name)s %(asctime)-15s %(filename)s:%(lineno)d %(message)s'
logging.basicConfig(format=FORMAT)
logger = logging.getLogger()
logger.setLevel(logging.DEBUG)

def info(environ, protocol):
    protocol(
        "200 OK",
        [('content-type', 'application/json')]
    )
    return [json.dumps({
        'scheme': environ['wsgi.url_scheme'],
        'method': environ['REQUEST_METHOD'],
        'path': environ["PATH_INFO"],
        'query_string': environ["QUERY_STRING"],
        'content_length': int(environ['CONTENT_LENGTH']),
        'headers': {k: v for k, v in environ.items() if k.startswith("HTTP_")}
    }).encode("utf8")]


def echo(environ, protocol):
    response = environ['wsgi.input'].read()
    protocol(
        '200 OK',
        [('content-type', 'text/plain; charset=utf-8'), ("content-length", str(len(response)))]
    )

    return [response]

def echo_iter(environ, protocol):
    protocol(
        '200 OK',
        [('content-type', 'text/plain; charset=utf-8')]
    )
    for line in environ['wsgi.input']:
        yield line

def err_app(environ, protocol):
    1 / 0

def empty_app(environ, protocol):
    protocol(
        '204 OK',
        [('content-type', 'text/plain; charset=utf-8')]
    )

    return b''

def app(environ, protocol):
    return {
        "/info": info,
        "/echo": echo,
        "/echo_iter": echo_iter,
        "/err_app": err_app,
        "/empty_app": empty_app,
        "/": echo,
        "": echo,
    }[environ["PATH_INFO"]](environ, protocol)


rustgi.serve(
    app,
    rustgi.RustgiConfig()
        .set_address("0.0.0.0:8000")
        .set_max_body_size(1024 * 66 * 7)
)
