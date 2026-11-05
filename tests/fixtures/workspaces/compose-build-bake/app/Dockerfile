FROM busybox:1.37
RUN --mount=type=secret,id=tok mkdir -p /www && cat /run/secrets/tok > /www/token && echo ok > /www/index.txt
CMD ["httpd", "-f", "-p", "8080", "-h", "/www"]
