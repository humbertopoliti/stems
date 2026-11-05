# Lives outside the build context (`app/`); granted through `allow_read`.
FROM busybox:1.37
COPY index.txt /www/index.txt
RUN --mount=type=secret,id=tok cat /run/secrets/tok > /www/token
CMD ["httpd", "-f", "-p", "8080", "-h", "/www"]
