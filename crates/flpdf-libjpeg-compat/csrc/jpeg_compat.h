#ifndef FLPDF_JPEG_COMPAT_H
#define FLPDF_JPEG_COMPAT_H

#include <stddef.h>

typedef int (*flpdf_jpeg_scanline_callback)(
    void *user,
    const unsigned char *row,
    size_t row_len);

int flpdf_jpeg_decode_scanlines(
    const unsigned char *data,
    size_t data_len,
    flpdf_jpeg_scanline_callback callback,
    void *user,
    char *error_message,
    size_t error_message_len);

#endif
