LARGE_CHUNK = [b"X" * 1024]

def application(environment, start_response):
    start_response(
        '200 OK',  # Status
        [('Content-type', 'text/plain'), ('Content-Length', '1024')]  # Headers
    )
    return [LARGE_CHUNK]
