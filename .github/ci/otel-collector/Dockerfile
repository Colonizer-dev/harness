# The collector for the observability conformance run (#864), pinned by digest so the job is
# reproducible whatever the host architecture is. Tag AND digest, like images/colony-node/Dockerfile:
# dependabot resolves a digest-only pin against latest, so the tag is what keeps its weekly bumps
# on this version line. Verify the digest with:
#   curl -s https://hub.docker.com/v2/repositories/otel/opentelemetry-collector-contrib/tags/0.162.0 | jq -r .digest
FROM otel/opentelemetry-collector-contrib:0.162.0@sha256:39923a8e431bd1f57be82411999d389fcfe40857492e4365456d97a4c1f74be6

COPY conformance.yaml /etc/otelcol/conformance.yaml

CMD ["--config=/etc/otelcol/conformance.yaml"]
