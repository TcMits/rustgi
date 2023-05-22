def application(environment, start_response):
    start_response(
        '200 OK',  # Status
        [('Content-type', 'text/plain'), ('Content-Length', '2')]  # Headers
    )
    return [b'OK']
