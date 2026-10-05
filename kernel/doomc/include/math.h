// MFK freestanding math.h (MIT). The engine is fixed-point; no libm
// calls are used. Present so vendored includes resolve.
#ifndef MFK_MATH_H
#define MFK_MATH_H

static inline double mfk_fabs(double x) { return x < 0 ? -x : x; }
static inline float mfk_fabsf(float x) { return x < 0 ? -x : x; }
#define fabs(x) mfk_fabs(x)
#define fabsf(x) mfk_fabsf(x)

#endif
