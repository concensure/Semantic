def normalize_url(url: str) -> str:
    return url.strip().lower()


def retry_request(limit: int) -> int:
    value = normalize_url("HTTP://EXAMPLE.COM")
    if not value:
        return 0
    return limit


class RetryPolicy:
    def __init__(self, attempts: int):
        self.attempts = attempts

    def apply(self) -> int:
        return retry_request(self.attempts)
