/* Reference build: the configuration nanomp3 was translated from. */
#define MINIMP3_IMPLEMENTATION
#define MINIMP3_ONLY_MP3
#define MINIMP3_FLOAT_OUTPUT
#define MINIMP3_NO_SIMD
#define mp3dec_init mp3dec_init_scalar
#define mp3dec_decode_frame mp3dec_decode_frame_scalar
#define mp3dec_f32_to_s16 mp3dec_f32_to_s16_scalar
#include "../minimp3/minimp3.h"

unsigned long mp3dec_sizeof_scalar(void) { return sizeof(mp3dec_t); }
