"""HQ LTX-2.5 with supplied audio frozen through both res_2s stages.

Uses the pinned MLX runtime's audio encoder, conditioning, streamed transformer
swap, sampler and decoder. No generated or VAE-reconstructed audio is delivered.
"""
from __future__ import annotations

import mlx.core as mx
from ltx_core_mlx.components.guiders import MultiModalGuiderParams, create_multimodal_guider_factory
from ltx_core_mlx.components.patchifiers import compute_video_latent_shape, snap_output_dimensions
from ltx_core_mlx.model.transformer.model import X0Model
from ltx_core_mlx.utils.memory import aggressive_cleanup
from ltx_core_mlx.utils.positions import compute_video_positions, compute_audio_positions
from ltx_pipelines_mlx.a2vid_two_stage import encode_source_audio, decode_with_source_audio
from ltx_pipelines_mlx.scheduler import STAGE_2_SIGMAS, ltx2_schedule, shorten_schedule
from ltx_pipelines_mlx.ti2vid_two_stages_hq import TI2VidTwoStagesHQPipeline
from ltx_pipelines_mlx.utils._orchestration import combined_image_conditionings
from ltx_pipelines_mlx.utils.args import ImageConditioningInput, resolve_frame_indices
from ltx_pipelines_mlx.utils.blocks import snap_num_frames
from ltx_pipelines_mlx.utils.helpers import create_noised_state
from ltx_pipelines_mlx.utils.samplers import res2s_denoise_loop


class AudioHQPipeline(TI2VidTwoStagesHQPipeline):
    def generate_and_save(self, prompt, output_path, audio_path, height, width, num_frames, *,
                          frame_rate, seed=42, stage1_steps=15, stage2_steps=3, cfg_scale=3.0,
                          stg_scale=0.0, image=None, images=None, negative_prompt=None,
                          enable_teacache=False):
        if enable_teacache:
            raise ValueError("Audio-conditioned HQ does not use TeaCache.")
        num_frames = snap_num_frames(num_frames)
        audio = encode_source_audio(self, audio_path, num_frames=num_frames,
                                    frame_rate=frame_rate, start_time=0.0,
                                    max_duration=num_frames / frame_rate)
        positive_video, positive_audio, negative_video, negative_audio = self._encode_text_with_negative(
            prompt, negative_prompt)
        if self.dit is None:
            self.dit = self._load_dev_transformer()
        height, width = snap_output_dimensions(height, width, two_stage=True)
        frames, half_h, half_w = compute_video_latent_shape(num_frames, height // 2, width // 2)
        full_h, full_w = half_h * 2, half_w * 2
        resolved = list(images or [])
        if image is not None and not resolved:
            resolved = [ImageConditioningInput.at(image, 0, 1.0)]
        resolved = self.image_conditioner.resolve_crf(resolve_frame_indices(resolved, num_frames))

        def conditionings(encoder, h, w):
            return combined_image_conditionings(resolved, enc_h=h * 32, enc_w=w * 32,
                                                spatial_dims=(frames, h, w), video_encoder=encoder,
                                                frame_rate=frame_rate) if resolved else []

        first_conditions = self.image_conditioner(
            lambda encoder: conditionings(encoder, half_h, half_w), free_after=self.low_memory
        ) if resolved else []
        audio_positions = compute_audio_positions(audio.shape[1])

        def frozen_audio(spatial_dims):
            return create_noised_state(base_shape=audio.shape, conditionings=[], spatial_dims=spatial_dims,
                                       positions=audio_positions, seed=seed + 1,
                                       initial_latent=audio, frozen=True)

        first_video = create_noised_state(
            base_shape=(1, frames * half_h * half_w, 128), conditionings=first_conditions,
            spatial_dims=(frames, half_h, half_w),
            positions=compute_video_positions(frames, half_h, half_w, frame_rate=frame_rate),
            seed=seed, sigma=1.0, initial_latent=None, legacy_scalar_blend=True)
        first_audio = frozen_audio((frames, half_h, half_w))
        self._pre_denoise_flush(first_video, first_audio)
        first = res2s_denoise_loop(
            model=X0Model(self.dit), video_state=first_video, audio_state=first_audio,
            video_text_embeds=positive_video, audio_text_embeds=positive_audio,
            sigmas=ltx2_schedule(stage1_steps, num_tokens=frames * half_h * half_w),
            video_guider_factory=create_multimodal_guider_factory(MultiModalGuiderParams(
                cfg_scale=cfg_scale, stg_scale=stg_scale, rescale_scale=0.45,
                modality_scale=3.0, stg_blocks=[]), negative_context=negative_video),
            audio_guider_factory=create_multimodal_guider_factory(MultiModalGuiderParams(),
                                                                 negative_context=negative_audio),
            on_step=self._stepwise_hook(frames, half_h, half_w, stage=1))
        if self.low_memory:
            aggressive_cleanup()
        # On our LTX-2.5 streamed pack this swaps to the existing distilled
        # transformer, exactly as the normal HQ path does.
        self._fuse_distilled_lora(self.dit)
        if self.upsampler is None:
            self._load_upsampler()
        half = self.video_patchifier.unpatchify(first.video_latent[:, :frames * half_h * half_w],
                                               (frames, half_h, half_w))

        def upscale(encoder):
            denormalized = encoder.denormalize_latent(half.transpose(0, 2, 3, 4, 1)).transpose(0, 4, 1, 2, 3)
            upscaled = self.upsampler(denormalized)
            normalized = encoder.normalize_latent(upscaled.transpose(0, 2, 3, 4, 1)).transpose(0, 4, 1, 2, 3)
            mx.eval(normalized)
            return normalized, conditionings(encoder, full_h, full_w)

        upscaled, second_conditions = self.image_conditioner(upscale, free_after=self.low_memory)
        if self.low_memory:
            self.upsampler = None
            aggressive_cleanup()
        video_tokens, _ = self.video_patchifier.patchify(upscaled)
        sigmas = shorten_schedule(STAGE_2_SIGMAS, stage2_steps, keep="tail")
        second_video = create_noised_state(
            base_shape=video_tokens.shape, conditionings=second_conditions,
            spatial_dims=(frames, full_h, full_w),
            positions=compute_video_positions(frames, full_h, full_w, frame_rate=frame_rate),
            seed=seed + 2, sigma=sigmas[0], initial_latent=video_tokens, legacy_scalar_blend=True)
        second_audio = frozen_audio((frames, full_h, full_w))
        self._pre_denoise_flush(second_video, second_audio)
        second = res2s_denoise_loop(
            model=X0Model(self.dit), video_state=second_video, audio_state=second_audio,
            video_text_embeds=positive_video, audio_text_embeds=positive_audio, sigmas=sigmas,
            on_step=self._stepwise_hook(frames, full_h, full_w, stage=2))
        latent = self.video_patchifier.unpatchify(second.video_latent[:, :frames * full_h * full_w],
                                                 (frames, full_h, full_w))
        if self.low_memory:
            self.dit = None
            self.prompt_encoder.free()
            self.image_conditioner.free()
            self._loaded = False
            aggressive_cleanup()
        self._load_decoders()
        decode_with_source_audio(self, latent, output_path, audio_path=audio_path, start_time=0.0,
                                 num_frames=num_frames, frame_rate=frame_rate)
        if self.low_memory:
            self.vae_decoder = self.audio_decoder = self.vocoder = None
            aggressive_cleanup()
        return output_path
