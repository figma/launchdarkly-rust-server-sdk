pub fn is_http_success(status: u16) -> bool {
    status < 300
}

pub fn is_http_client_error(status: u16) -> bool {
    status >= 400 && status < 500
}

pub fn is_http_error_recoverable(status: u16) -> bool {
    if !is_http_client_error(status) {
        return true;
    }

    matches!(status, 400 | 408 | 429)
}
