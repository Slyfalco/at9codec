// Package at9codec is a cgo binding to the at9codec shared library.
//
// Build/link (adjust the paths or install the lib + header system-wide):
//
//	#cgo CFLAGS points at ../../include and LDFLAGS at ../../target/release.
//	At run time the shared library must be on the loader path
//	(LD_LIBRARY_PATH / DYLD_LIBRARY_PATH, an rpath, or a system dir; on Windows
//	put at9codec.dll next to the executable).
//
// Example:
//
//	pcm, ch, rate, _ := at9codec.Decode(at9bytes, nil)
//	out, _ := at9codec.EncodeWav(wavbytes, at9codec.Options{
//	        LoopStart:   0,                     // whole-track loop
//	        SizeProfile: at9codec.SizeVitaBgm,  // codec owns the limit
//	        Progress:    func(d, t uint32) { fmt.Printf("\r%d/%d", d, t) },
//	})
package at9codec

/*
#cgo CFLAGS: -I${SRCDIR}/../../include
#cgo LDFLAGS: -L${SRCDIR}/../../target/release -lat9codec
#include <stdlib.h>
#include "at9codec.h"

// Trampoline implemented in Go (see //export goAt9Progress) so the options
// struct's function-pointer field points at real C-callable code.
extern void goAt9Progress(void* user, uint32_t done, uint32_t total);
*/
import "C"

import (
	"fmt"
	"runtime/cgo"
	"unsafe"
)

// Size profiles: name a console target and the codec applies its own byte cap.
const (
	SizeNone    = 0
	SizeVitaBgm = 1 // PS Vita system/theme BGM (under 5 MB)
	SizePsCross = 2 // tighter 2 MiB cap for PS4/PS5 cross-support
)

// Options controls an encode. The zero value is "best quality, no loop, uncapped".
type Options struct {
	Bitrate     uint32 // 0 = best standard default
	SizeProfile uint32 // Size*; non-zero makes the codec apply its own cap (overrides MaxBytes)
	MaxBytes    uint64 // custom cap in bytes; 0 = no cap; ignored if SizeProfile set
	LoopStart   int32  // <0 none, 0 whole track, >0 from this sample
	LoopEnd     int32  // <=0 to track end, >0 inclusive last sample
	Progress    func(done, total uint32)
}

// ProfileMaxBytes returns the codec's byte cap for a size profile (Size*); 0 = no
// cap/unknown. Reads the limit from the library so you never hard-code it.
func ProfileMaxBytes(sizeProfile uint32) uint64 {
	return uint64(C.at9_profile_max_bytes(C.uint32_t(sizeProfile)))
}

//export goAt9Progress
func goAt9Progress(user unsafe.Pointer, done, total C.uint32_t) {
	if user == nil {
		return
	}
	if cb, ok := cgo.Handle(uintptr(user)).Value().(func(uint32, uint32)); ok && cb != nil {
		cb(uint32(done), uint32(total))
	}
}

// Version returns the library version string.
func Version() string { return C.GoString(C.at9_version()) }

// Decode decodes AT9 bytes to interleaved little-endian 16-bit PCM.
func Decode(at9 []byte, progress func(done, total uint32)) (pcm []byte, channels, sampleRate int, err error) {
	if len(at9) == 0 {
		return nil, 0, 0, fmt.Errorf("empty input")
	}
	var opts C.At9DecodeOptions
	opts.struct_size = C.uint32_t(unsafe.Sizeof(opts))
	if progress != nil {
		h := cgo.NewHandle(progress)
		defer h.Delete()
		opts.progress = C.At9ProgressCb(C.goAt9Progress)
		opts.user = unsafe.Pointer(uintptr(h))
	}
	var out *C.int16_t
	var count C.size_t
	var ch C.uint16_t
	var sr C.uint32_t
	rc := C.at9_decode((*C.uint8_t)(unsafe.Pointer(&at9[0])), C.size_t(len(at9)), &opts,
		&out, &count, &ch, &sr)
	if rc != C.AT9_OK {
		return nil, 0, 0, fmt.Errorf("at9_decode failed: %d", int(rc))
	}
	defer C.at9_free_pcm(out, count)
	pcm = C.GoBytes(unsafe.Pointer(out), C.int(count)*2)
	return pcm, int(ch), int(sr), nil
}

// EncodeWav encodes a WAV buffer to AT9 bytes.
func EncodeWav(wav []byte, o Options) ([]byte, error) {
	if len(wav) == 0 {
		return nil, fmt.Errorf("empty input")
	}
	var opts C.At9EncodeOptions
	opts.struct_size = C.uint32_t(unsafe.Sizeof(opts))
	opts.bitrate = C.uint32_t(o.Bitrate)
	opts.max_bytes = C.size_t(o.MaxBytes)
	opts.loop_start = C.int32_t(o.LoopStart)
	opts.loop_end = C.int32_t(o.LoopEnd)
	opts.size_profile = C.uint32_t(o.SizeProfile)
	if o.Progress != nil {
		h := cgo.NewHandle(o.Progress)
		defer h.Delete()
		opts.progress = C.At9ProgressCb(C.goAt9Progress)
		opts.user = unsafe.Pointer(uintptr(h))
	}
	var out *C.uint8_t
	var outLen C.size_t
	rc := C.at9_encode_wav((*C.uint8_t)(unsafe.Pointer(&wav[0])), C.size_t(len(wav)), &opts,
		&out, &outLen)
	if rc == C.AT9_ERR_TOO_LARGE {
		return nil, fmt.Errorf("track too long to fit the size limit")
	}
	if rc != C.AT9_OK {
		return nil, fmt.Errorf("at9_encode_wav failed: %d", int(rc))
	}
	defer C.at9_free_at9(out, outLen)
	return C.GoBytes(unsafe.Pointer(out), C.int(outLen)), nil
}
