import json
import logging
import rustgi


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
    protocol(
        '200 OK',
        [('content-type', 'text/plain; charset=utf-8')]
    )
    return [environ['wsgi.input'].read()]

def echo_iter(environ, protocol):
    protocol(
        '200 OK',
        [('content-type', 'text/plain; charset=utf-8')]
    )
    for line in environ['wsgi.input']:
        yield line

def echo_readline(environ, protocol):
    protocol(
        '200 OK',
        [('content-type', 'text/plain; charset=utf-8')]
    )

    while (line := environ['wsgi.input'].readline()):
        yield line

def echo_readlines(environ, protocol):
    protocol(
        '200 OK',
        [('content-type', 'text/plain; charset=utf-8')]
    )

    for line in environ['wsgi.input'].readlines():
        yield line

def err_app(environ, protocol):
    1 / 0

def app(environ, protocol):
    return {
        "/info": info,
        "/echo": echo,
        "/echo_iter": echo_iter,
        "/echo_readline": echo_readline,
        "/echo_readlines": echo_readlines,
        "/err_app": err_app
    }[environ["PATH_INFO"]](environ, protocol)


rustgi.serve(app, rustgi.RustgiConfig())
