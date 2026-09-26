# Container image for shop-api (stdlib only, no dependencies to install).
FROM python:3-slim
WORKDIR /app
COPY app.py ./
COPY migrations ./migrations
ENV HOST=0.0.0.0 PORT=8080 PYTHONUNBUFFERED=1
EXPOSE 8080
CMD ["python3", "app.py"]
