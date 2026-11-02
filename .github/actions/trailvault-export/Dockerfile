FROM ubuntu:24.04
RUN apt-get update && apt-get install -y --no-install-recommends git curl zip ca-certificates && rm -rf /var/lib/apt/lists
COPY trailvault /usr/local/bin/trailvault
COPY register.sh export.sh /
RUN chmod 755 /register.sh /export.sh /usr/local/bin/trailvault
