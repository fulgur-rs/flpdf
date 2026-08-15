#include "jpeg_compat.h"

#include <stdio.h>
#include <jpeglib.h>
#include <jerror.h>

#include <limits.h>
#include <setjmp.h>
#include <stdint.h>
#include <string.h>

#if !defined(BITS_IN_JSAMPLE)
#error "flpdf requires a libjpeg header that declares BITS_IN_JSAMPLE"
#endif

#if BITS_IN_JSAMPLE != 8
#error "flpdf requires an 8-bit system libjpeg build"
#endif

/* qpdf Pl_DCT uses only the API subset already present in libjpeg 6b. */
#if !defined(JPEG_LIB_VERSION) || (JPEG_LIB_VERSION < 62)
#error "flpdf requires the libjpeg 6b-compatible API surface"
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

struct flpdf_jpeg_source {
    struct jpeg_source_mgr public;
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
flpdf_jpeg_abort_with_message(j_decompress_ptr cinfo, const char *message)
{
    struct flpdf_jpeg_error_manager *error_manager =
        (struct flpdf_jpeg_error_manager *)cinfo->err;

    flpdf_copy_error_message(
        error_manager->error_message,
        error_manager->error_message_len,
        message);
    longjmp(error_manager->jump_buffer, 1);
}

static void
flpdf_jpeg_init_source(j_decompress_ptr cinfo)
{
    (void)cinfo;
}

static boolean
flpdf_jpeg_fill_input_buffer(j_decompress_ptr cinfo)
{
    /* qpdf's whole-buffer source reports this exact error instead of adding an EOI marker. */
    flpdf_jpeg_abort_with_message(cinfo, "invalid jpeg data reading from buffer");
    return TRUE;
}

static void
flpdf_jpeg_skip_input_data(j_decompress_ptr cinfo, long num_bytes)
{
    size_t available;

    if (num_bytes < 0) {
        flpdf_jpeg_abort_with_message(
            cinfo,
            "reading jpeg: jpeg library requested skipping a negative number of bytes");
        return;
    }

    available = cinfo->src->bytes_in_buffer;
    if ((size_t)num_bytes > available) {
        /* Match qpdf: consume the remaining buffer and let the next fill report EOF. */
        if (available != 0) {
            cinfo->src->next_input_byte += available;
        }
        cinfo->src->bytes_in_buffer = 0;
        return;
    }

    if (num_bytes != 0) {
        cinfo->src->next_input_byte += (size_t)num_bytes;
    }
    cinfo->src->bytes_in_buffer -= (size_t)num_bytes;
}

static void
flpdf_jpeg_term_source(j_decompress_ptr cinfo)
{
    (void)cinfo;
}

static void
flpdf_jpeg_buffer_src(
    j_decompress_ptr cinfo,
    const unsigned char *data,
    size_t data_len)
{
    struct flpdf_jpeg_source *source =
        (struct flpdf_jpeg_source *)(*cinfo->mem->alloc_small)(
            (j_common_ptr)cinfo,
            JPOOL_PERMANENT,
            sizeof(struct flpdf_jpeg_source));

    source->public.init_source = flpdf_jpeg_init_source;
    source->public.fill_input_buffer = flpdf_jpeg_fill_input_buffer;
    source->public.skip_input_data = flpdf_jpeg_skip_input_data;
    source->public.resync_to_restart = jpeg_resync_to_restart;
    source->public.term_source = flpdf_jpeg_term_source;
    source->public.next_input_byte = data;
    source->public.bytes_in_buffer = data_len;
    cinfo->src = &source->public;
}

static void
flpdf_jpeg_format_error(
    j_common_ptr common,
    char *destination,
    size_t destination_len)
{
    char diagnostic[JMSG_LENGTH_MAX];

    switch (common->err->msg_code) {
    case JERR_NO_IMAGE:
        flpdf_copy_error_message(
            destination,
            destination_len,
            "JPEG datastream contains no image");
        return;
    case JERR_BAD_PRECISION:
        (void)snprintf(
            diagnostic,
            sizeof(diagnostic),
            "Unsupported JPEG data precision %d",
            common->err->msg_parm.i[0]);
        flpdf_copy_error_message(destination, destination_len, diagnostic);
        return;
    case JERR_NO_SOI:
        (void)snprintf(
            diagnostic,
            sizeof(diagnostic),
            "Not a JPEG file: starts with 0x%02x 0x%02x",
            (unsigned int)(common->err->msg_parm.i[0] & 0xff),
            (unsigned int)(common->err->msg_parm.i[1] & 0xff));
        flpdf_copy_error_message(destination, destination_len, diagnostic);
        return;
    case JERR_SOF_NO_SOS:
        flpdf_copy_error_message(
            destination,
            destination_len,
            "Invalid JPEG file structure: missing SOS marker");
        return;
    case JERR_SOF_UNSUPPORTED:
        (void)snprintf(
            diagnostic,
            sizeof(diagnostic),
            "Unsupported JPEG process: SOF type 0x%02x",
            (unsigned int)(common->err->msg_parm.i[0] & 0xff));
        flpdf_copy_error_message(destination, destination_len, diagnostic);
        return;
    case JERR_UNKNOWN_MARKER:
        (void)snprintf(
            diagnostic,
            sizeof(diagnostic),
            "Unsupported marker type 0x%02x",
            (unsigned int)(common->err->msg_parm.i[0] & 0xff));
        flpdf_copy_error_message(destination, destination_len, diagnostic);
        return;
    default:
        (*common->err->format_message)(common, diagnostic);
        flpdf_copy_error_message(destination, destination_len, diagnostic);
        return;
    }
}

static void
flpdf_jpeg_error_exit(j_common_ptr common)
{
    struct flpdf_jpeg_error_manager *error_manager =
        (struct flpdf_jpeg_error_manager *)common->err;

    if ((error_manager->error_message != NULL) && (error_manager->error_message_len > 0)) {
        flpdf_jpeg_format_error(
            common,
            error_manager->error_message,
            error_manager->error_message_len);
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
    flpdf_jpeg_buffer_src(&cinfo, data, data_len);
    (void)jpeg_read_header(&cinfo, TRUE);
    (void)jpeg_calc_output_dimensions(&cinfo);

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
    if (jpeg_start_decompress(&cinfo) == FALSE) {
        flpdf_copy_error_message(
            error_message,
            error_message_len,
            "jpeg_start_decompress returned false");
        jpeg_destroy_decompress(&cinfo);
        return FLPDF_JPEG_CODEC_ERROR;
    }

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

    if (jpeg_finish_decompress(&cinfo) == FALSE) {
        flpdf_copy_error_message(
            error_message,
            error_message_len,
            "jpeg_finish_decompress returned false");
        jpeg_destroy_decompress(&cinfo);
        return FLPDF_JPEG_CODEC_ERROR;
    }
    jpeg_destroy_decompress(&cinfo);
    return FLPDF_JPEG_SUCCESS;
}
