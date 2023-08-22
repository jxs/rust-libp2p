from rust:1.71-slim-buster

COPY . /libp2p

CMD ["/usr/local/cargo/bin/cargo", "run", "--manifest-path", "/libp2p/examples/upnp/Cargo.toml"]
