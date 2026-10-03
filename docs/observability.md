# Observability

Send the harness's logs, traces and metrics to a backend you run or subscribe to, over
OpenTelemetry's OTLP protocol, or keep them in capped files on the mothership. It is off until you
configure it, and it holds no credential of its own — the header a backend wants lives in a secret.

- [Settings and privacy](observability/settings.md): the module's settings, how to turn it on and
  off, what is sent, and what is never sent.
