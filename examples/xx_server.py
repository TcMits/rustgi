import rustgi

RESPONSE = b'x' * 2

def application(_, start_response):
    start_response(
        '200 OK',  # Status
        [('Content-type', 'text/plain'), ('Content-Length', str(len(RESPONSE)))]  # Headers
    )
    return [RESPONSE]


rustgi.serve(
    application,
    rustgi.RustgiConfig().set_address("0.0.0.0:8000"),
)
