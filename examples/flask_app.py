from flask import Flask
import rustgi

app = Flask(__name__)


@app.route("/")
def hello_world():
    return "<p>Hello, World!</p>"


if __name__ == "__main__":
    rustgi.serve(
        app,
        rustgi.RustgiConfig()
        .set_address("0.0.0.0:8000")
        .set_max_body_size(10 * 1024 * 1024),  # 10MB
    )
