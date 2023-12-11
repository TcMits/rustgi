import requests
import pytest
import socket
import errno

HOST_WITHOUT_PORT = "localhost"
PORT = 8000
HOST = HOST_WITHOUT_PORT + ":" + str(PORT)
HOST_WITH_SCHEME = "http://" + HOST

LARGE_BODY = "xxxxxx\n" * 1024


def test_wsgi():
    r = requests.post(HOST_WITH_SCHEME + "/info?test=true", data="world")
    assert r.status_code == 200
    assert r.headers["content-type"] == "application/json"
    r_json = r.json()
    assert r_json["scheme"] == "http"
    assert r_json["method"] == "POST"
    assert r_json["path"] == "/info"
    assert r_json["query_string"] == "test=true"
    assert r_json["content_length"] == 5
    assert r_json["headers"]["HTTP_HOST"] == HOST


def test_wsgi_body():
    r = requests.post(HOST_WITH_SCHEME + "/echo", data="hello\nworld")
    assert r.status_code == 200
    assert r.text == "hello\nworld"


def test_wsgi_body_iter():
    r = requests.post(HOST_WITH_SCHEME + "/echo_iter", data="hello\nworld")
    assert r.status_code == 200
    assert r.text == "hello\nworld"


def test_wsgi_body_large():
    r = requests.post(HOST_WITH_SCHEME + "/echo_iter", data=LARGE_BODY)
    assert r.status_code == 200
    assert r.text == LARGE_BODY


def test_wsgi_err():
    with pytest.raises(requests.exceptions.ConnectionError):
        requests.get(HOST_WITH_SCHEME + "/err_app")


def test_wsgi_empty():
    r = requests.get(HOST_WITH_SCHEME + "/empty_app")
    assert r.status_code == 204


def test_wsgi_chunked():
    r = requests.post(
        HOST_WITH_SCHEME + "/echo_iter", data=("xxxxxx\n" for _ in range(1024))
    )
    assert r.status_code == 200
    assert r.text == LARGE_BODY

    r = requests.post(
        HOST_WITH_SCHEME + "/echo_iter", data=("xxxxxx\n" for _ in range(1024 * 65))
    )
    assert r.status_code == 200
    assert r.text == LARGE_BODY * 65


def test_max_body_size():
    r = requests.post(HOST_WITH_SCHEME + "/echo", data=("xxxxxx\n" for _ in range(1024 * 67)))
    assert r.status_code == 413

def test_wsgi_100_continue():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as client:
        client.connect((HOST_WITHOUT_PORT, PORT))
        request = (
            "GET /echo HTTP/1.1\r\n"
            "Expect: 100-continue\r\n"
            "Content-Length: 5\r\n\r\n"
        )

        client.send(request.encode("utf8"))

        response = client.recv(1024)
        assert response == b"HTTP/1.1 100 Continue\r\n\r\n"

        client.send("hello".encode("utf8"))
        result = client.recv(1024)
        assert result.endswith(b"hello")
