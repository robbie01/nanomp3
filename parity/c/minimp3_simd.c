/* Performance build: upstream defaults with SSE/NEON intrinsics enabled. */
#define MINIMP3_IMPLEMENTATION
#define MINIMP3_ONLY_MP3
#define MINIMP3_FLOAT_OUTPUT
#define mp3dec_init mp3dec_init_simd
#define mp3dec_decode_frame mp3dec_decode_frame_simd
#define mp3dec_f32_to_s16 mp3dec_f32_to_s16_simd
#include "../minimp3/minimp3.h"

unsigned long mp3dec_sizeof_simd(void) { return sizeof(mp3dec_t); }
