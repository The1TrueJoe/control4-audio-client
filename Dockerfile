# Build + test image: docker build -t c4ac . && docker run --rm c4ac
FROM rust:1-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends \
      libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
      gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
      python3-gi gir1.2-gstreamer-1.0 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release && cargo test --release
CMD ["python3", "test/selfcheck.py", "target/release/c4-audio-client"]
