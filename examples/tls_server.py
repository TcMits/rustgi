import rustgi
from app import application

rustgi.serve(
    application,
    rustgi.RustgiConfig()
        .set_address("0.0.0.0:443")
        .set_tls_config(
            rustgi.TLSConfig()
                .set_certs("tls/cert.pem")
                .set_key("tls/key.rsa")
        ),
)
