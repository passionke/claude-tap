# Runtime image for proxy-only + live viewer (see docker-compose.yml).
FROM python:3.12-slim-bookworm

WORKDIR /app

COPY pyproject.toml README.md ./
COPY claude_tap ./claude_tap

# setuptools-scm needs a version when .git is not copied into the image.
ENV SETUPTOOLS_SCM_PRETEND_VERSION=0.0.0+docker

RUN pip install --no-cache-dir --upgrade pip \
    && pip install --no-cache-dir .

ENV PYTHONUNBUFFERED=1

EXPOSE 8080 3000

CMD ["claude-tap", "--tap-no-launch", "--tap-host", "0.0.0.0", "--tap-port", "8080", "--tap-live", "--tap-live-port", "3000", "--tap-output-dir", "/data/traces", "--tap-no-update-check", "--tap-no-auto-update"]
