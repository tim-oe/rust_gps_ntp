FROM python:3.12-slim

WORKDIR /opt/ntp-load
COPY scripts/ntp_common.py scripts/load_ntp.py ./

ENTRYPOINT ["python3", "load_ntp.py"]
