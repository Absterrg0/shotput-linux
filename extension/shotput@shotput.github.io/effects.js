// GPU masks for rounded thumbnails and the round magnifier.

import Cogl from 'gi://Cogl';
import GObject from 'gi://GObject';
import Shell from 'gi://Shell';

/** Clips an actor to a rounded rectangle, antialiased. */
export const RoundedMask = GObject.registerClass(
class ShotputRoundedMask extends Shell.GLSLEffect {
    _init(radius) {
        super._init();
        this._radius = radius;
        this._sizeLocation = this.get_uniform_location('size');
        this._radiusLocation = this.get_uniform_location('radius');
        this._lastSize = null;
    }

    vfunc_build_pipeline() {
        this.add_glsl_snippet(Cogl.SnippetHook.FRAGMENT, 'uniform vec2 size;\nuniform float radius;\n', `
            vec2 p = cogl_tex_coord_in[0].xy * size;
            vec2 q = abs(p - size * 0.5) - (size * 0.5 - vec2(radius));
            float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
            cogl_color_out *= clamp(0.5 - d, 0.0, 1.0);
        `, false);
    }

    vfunc_paint_target(node, paintContext) {
        const actor = this.get_actor();
        if (actor) {
            const [w, h] = actor.get_size();
            const scale = actor.get_resource_scale?.() ?? 1;
            const key = `${w}x${h}@${scale}`;
            if (key !== this._lastSize) {
                this._lastSize = key;
                this.set_uniform_float(this._sizeLocation, 2, [w * scale, h * scale]);
                this.set_uniform_float(this._radiusLocation, 1, [this._radius * scale]);
            }
        }
        super.vfunc_paint_target(node, paintContext);
    }
});

/** Clips an actor to the circle inscribed in it. */
export const CircleMask = GObject.registerClass(
class ShotputCircleMask extends Shell.GLSLEffect {
    vfunc_build_pipeline() {
        this.add_glsl_snippet(Cogl.SnippetHook.FRAGMENT, '', `
            float d = distance(cogl_tex_coord_in[0].xy, vec2(0.5, 0.5));
            cogl_color_out *= 1.0 - smoothstep(0.492, 0.5, d);
        `, false);
    }
});
