# TODO List

- Improvement of path matcher
- Option for rewriting path like
  ```
  https://example.com:8080/path/to -> http://backend:3030/any_path
  ```
  Currently, incoming path (`/path/to/`) is always preserved in the mapping process, i.e., mapped to `backend:3030/path/to`.
- Smaller footprint of docker image using musl
- Refactoring
- Options to serve custom http_error page.
- etc.
