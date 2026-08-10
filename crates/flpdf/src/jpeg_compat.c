#include "jpeg_compat.h"

#include <stdio.h>
#include <jpeglib.h>

#include <limits.h>
#include <setjmp.h>
#include <stdint.h>
#include <string.h>

#if BITS_IN_JSAMPLE != 8
#error "flpdf requires an 8-bit system libjpeg build"
#endif

enum {
    FLPDF_JPEG_SUCCESS = 0,
    FLPDF_JPEG_CODEC_ERROR = 1,
    FLPDF_JPEG_CALLBACK_ERROR = 2,
};

struct flpdf_jpeg_error_manager {
    struct jpeg_error_mgr public;
    jmp_buf jump_buffer;
    char *error_message;
    size_t error_message_len;
};

static void
flpdf_copy_error_message(char *destination, size_t destination_len, const char *message)
{
    size_t message_len;
    size_t copy_len;

    if ((destination == NULL) || (destination_len == 0)) {
        return;
    }

    message_len = strlen(message);
    copy_len = message_len;
    if (copy_len >= destination_len) {
        copy_len = destination_len - 1;
    }
    memcpy(destination, message, copy_len);
    destination[copy_len] = '\0';
}

static void
flpdf_jpeg_error_exit(j_common_ptr common)
{
    struct flpdf_jpeg_error_manager *error_manager =
        (struct flpdf_jpeg_error_manager *)common->err;

    if ((error_manager->error_message != NULL) && (error_manager->error_message_len > 0)) {
        char diagnostic[JMSG_LENGTH_MAX];

        (*common->err->format_message)(common, diagnostic);
        flpdf_copy_error_message(
            error_manager->error_message,
            error_manager->error_message_len,
            diagnostic);
    }
    longjmp(error_manager->jump_buffer, 1);
}

int
flpdf_jpeg_decode_scanlines(
    const unsigned char *data,
    size_t data_len,
    flpdf_jpeg_scanline_callback callback,
    void *user,
    char *error_message,
    size_t error_message_len)
{
    struct jpeg_decompress_struct cinfo = {0};
    struct flpdf_jpeg_error_manager error_manager;
    volatile int decompress_cleanup_needed = 0;

    if ((error_message != NULL) && (error_message_len > 0)) {
        error_message[0] = '\0';
    }
    if (callback == NULL) {
        return FLPDF_JPEG_CALLBACK_ERROR;
    }
    if ((data == NULL) && (data_len != 0)) {
        flpdf_copy_error_message(error_message, error_message_len, "JPEG input data is null");
        return FLPDF_JPEG_CODEC_ERROR;
    }
    if (data_len > (size_t)ULONG_MAX) {
        flpdf_copy_error_message(error_message, error_message_len, "JPEG input data is too large");
        return FLPDF_JPEG_CODEC_ERROR;
    }

    cinfo.err = jpeg_std_error(&error_manager.public);
    error_manager.public.error_exit = flpdf_jpeg_error_exit;
    error_manager.error_message = error_message;
    error_manager.error_message_len = error_message_len;

    if (setjmp(error_manager.jump_buffer) != 0) {
        if (decompress_cleanup_needed != 0) {
            jpeg_destroy_decompress(&cinfo);
        }
        return FLPDF_JPEG_CODEC_ERROR;
    }

    decompress_cleanup_needed = 1;
    jpeg_create_decompress(&cinfo);
    jpeg_mem_src(&cinfo, data, (unsigned long)data_len);
    (void)jpeg_read_header(&cinfo, TRUE);
    (void)jpeg_calc_output_dimensions(&cinfo);

    if ((cinfo.num_components != 1) && (cinfo.num_components != 3) &&
        (cinfo.num_components != 4)) {
        char diagnostic[64];

        (void)snprintf(
            diagnostic,
            sizeof(diagnostic),
            "unsupported JPEG component count %d",
            cinfo.num_components);
        flpdf_copy_error_message(error_message, error_message_len, diagnostic);
        jpeg_destroy_decompress(&cinfo);
        return FLPDF_JPEG_CODEC_ERROR;
    }

    if ((cinfo.output_components <= 0) ||
        ((size_t)cinfo.output_width > SIZE_MAX / (size_t)cinfo.output_components)) {
        flpdf_copy_error_message(
            error_message,
            error_message_len,
            "JPEG output scanline size is too large");
        jpeg_destroy_decompress(&cinfo);
        return FLPDF_JPEG_CODEC_ERROR;
    }

    size_t row_len = (size_t)cinfo.output_width * (size_t)cinfo.output_components;
    if (row_len > (size_t)UINT_MAX) {
        flpdf_copy_error_message(
            error_message,
            error_message_len,
            "JPEG output scanline size is too large");
        jpeg_destroy_decompress(&cinfo);
        return FLPDF_JPEG_CODEC_ERROR;
    }

    JSAMPARRAY row = (*cinfo.mem->alloc_sarray)(
        (j_common_ptr)&cinfo,
        JPOOL_IMAGE,
        (JDIMENSION)row_len,
        1);
    (void)jpeg_start_decompress(&cinfo);

    while (cinfo.output_scanline < cinfo.output_height) {
        JDIMENSION rows_read = jpeg_read_scanlines(&cinfo, row, 1);
        if (rows_read != 1) {
            flpdf_copy_error_message(
                error_message,
                error_message_len,
                "libjpeg returned no output scanline");
            jpeg_destroy_decompress(&cinfo);
            return FLPDF_JPEG_CODEC_ERROR;
        }
        if (callback(user, row[0], row_len) != 0) {
            jpeg_destroy_decompress(&cinfo);
            return FLPDF_JPEG_CALLBACK_ERROR;
        }
    }

    (void)jpeg_finish_decompress(&cinfo);
    jpeg_destroy_decompress(&cinfo);
    return FLPDF_JPEG_SUCCESS;
}
