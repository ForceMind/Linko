#ifndef LINKO_WINDOWS_H264_FACTORY_H
#define LINKO_WINDOWS_H264_FACTORY_H

#include "api/video_codecs/video_decoder_factory.h"
#include "api/video_codecs/video_encoder_factory.h"

std::unique_ptr<webrtc::VideoEncoderFactory> CreateMfEncoderFactory();
std::unique_ptr<webrtc::VideoDecoderFactory> CreateMfDecoderFactory();
bool MfCodecCallbacksPassed();

#endif
