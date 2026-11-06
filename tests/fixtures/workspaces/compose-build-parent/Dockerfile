FROM busybox:1.37
COPY marker.txt /www/index.txt
CMD ["httpd", "-f", "-p", "8080", "-h", "/www"]
