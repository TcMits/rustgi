import rustgi
from app import application

rustgi.serve(
    application,
    rustgi.RustgiConfig().set_address("0.0.0.0:8000"),
)
