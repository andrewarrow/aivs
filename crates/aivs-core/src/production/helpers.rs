//! Compatibility entry points; all decisions share the production implementation.
use super::*;
use crate::{media, storyboard};
pub(super) fn dispatch(ctx: &mut Context<'_>, operation: &str, args: &Value) -> Result<Value> {
    match operation {
        "production_write_json" => {
            save(&path(args, "path")?, &args["value"])?;
            Ok(Value::Null)
        }
        "production_model_config" => model_config(args["profile"].as_str().unwrap_or("distilled")),
        "production_publish" => {
            ctx.publish(&args["plan"], &path(args, "run_dir")?)?;
            Ok(Value::Null)
        }
        "production_plan_hash" => Ok(json!(ctx.plan_hash(&args["plan"])?)),
        "production_cache_key" => Ok(json!(preparation::cache_key(
            ctx,
            &args["request"],
            &args["profiles"]
        )?)),
        "production_rebind" => preparation::rebind(
            ctx,
            &args["director"],
            &args["final"],
            &args["boards"],
            &path(args, "run_dir")?,
        ),
        "production_board_prepare" => {
            boards::prepare(ctx, &args["plan"], &args["boards"], &path(args, "run_dir")?)
        }
        "production_board_references" => {
            boards::references(ctx, &args["plan"], &args["boards"], &path(args, "run_dir")?)
        }
        "production_board_accepted" => {
            boards::accepted(ctx, &args["plan"], &args["boards"], &path(args, "run_dir")?)
        }
        "production_review_policy" => Ok(json!(boards::review_policy(ctx)?)),
        "production_review_binding" => boards::review_binding(ctx, &args["plan"], &args["boards"]),
        "production_reference_generate" => boards::generate_references(
            ctx,
            &args["plan"],
            &args["profiles"],
            &path(args, "run_dir")?,
            args["seed"].as_i64().ok_or("Invalid seed.")?,
            args.get("repair_ids").unwrap_or(&json!([])),
            args["round_number"].as_u64().unwrap_or(0),
        ),
        "production_regenerate" => boards::regenerate(
            ctx,
            &args["plan"],
            &args["profiles"],
            &path(args, "run_dir")?,
            args["seed"].as_i64().ok_or("Invalid seed.")?,
            args["shot_index"].as_u64().ok_or("Invalid shot index.")? as usize,
            text(args, "reason")?,
            &path(args, "directory")?,
            &args["prior_image"],
        ),
        "production_frame_request" => boards::frame_request(
            ctx,
            &args["plan"],
            &args["shot"],
            &args["profiles"],
            &path(args, "run_dir")?,
            &args["prior"],
            args["number"].as_u64().ok_or("Invalid shot number.")? as usize,
            args["seed"].as_i64().ok_or("Invalid seed.")?,
        ),
        "production_image_config" => boards::backend(ctx),
        "production_image_local" => boards::image_call(ctx, "imagegen_check_local", args.clone()),
        "production_image_check" => boards::image_call(ctx, "imagegen_check", args.clone()),
        "production_image_generate" => boards::image_call(ctx, "imagegen_generate", args.clone()),
        "production_review_shot" => review::shot(
            ctx,
            &args["plan"],
            args["shot_index"].as_u64().ok_or("Invalid review shot.")? as usize,
            &path(args, "clip")?,
            &path(args, "directory")?,
        ),
        "production_review_scene" => review::scene(
            ctx,
            &args["plan"],
            &args["shot_reviews"],
            &path(args, "directory")?,
        ),
        "production_adapt" => review::adapt(
            ctx,
            &args["plan"],
            args["shot_index"]
                .as_u64()
                .ok_or("Invalid adaptation shot.")? as usize,
            &args["previous_review"],
            &path(args, "directory")?,
            args["storyboard_mode"] == true,
        ),
        "production_anchors" => motion::anchors(&args["row"], &path(args, "run_dir")?),
        "production_motion_identity" => Ok(json!(motion::identity(ctx)?)),
        "production_assemble" => {
            crate::shot_renderer::storage("take_assemble", args, &ctx.cancelled)
        }
        "production_image_info" => media::dispatch(
            "prepare_source_image",
            &json!({"source":args["path"]}),
            &ctx.cancelled,
        ),
        "production_resolve_image" => Ok(json!(storyboard::resolve_image(
            &args["binding"],
            &path(args, "run_dir")?
        )?)),
        _ => Err(format!("Unknown production operation: {operation}")),
    }
}
