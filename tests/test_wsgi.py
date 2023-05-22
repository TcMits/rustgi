import httpx
import pytest

HOST = "0.0.0.0:8000"

LARGE_BODY = "xxxxxx\n" * 1024 * 64

def test_wsgi():
    with httpx.Client(base_url=f"http://{HOST}") as client:
        r = client.post("/info?test=true", data="world")
        assert r.status_code == 200
        assert r.headers["content-type"] == "application/json"
        r_json = r.json()
        assert r_json["scheme"] == "http"
        assert r_json["method"] == "POST"
        assert r_json["path"] == "/info"
        assert r_json["query_string"] == "test=true"
        assert r_json["content_length"] == 5
        assert r_json["headers"]["HTTP_HOST"] == HOST

        r = client.post("/echo", data="hello\nworld")
        assert r.status_code == 200
        assert r.text == "hello\nworld"

        r = client.post("/echo_iter", data="hello\nworld")
        assert r.status_code == 200
        assert r.text == "hello\nworld"

        r = client.post("/echo_readline", data="hello\nworld")
        assert r.status_code == 200
        assert r.text == "hello\nworld"

        r = client.post("/echo_readlines", data="hello\nworld")
        assert r.status_code == 200
        assert r.text == "hello\nworld"

        r = client.post("/echo_iter", data=LARGE_BODY)
        assert r.status_code == 200
        assert r.text == LARGE_BODY

        with pytest.raises(httpx.RemoteProtocolError):
            client.get("/err_app")
