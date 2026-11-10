bary.f (ei)r63.x, 0, r0.x
(sy)mad.f32 r1.z, c4.x, r0.z, c4.w
mad.f32 r1.w, c5.x, r0.z, c5.w
mad.f32 r2.x, c6.x, r0.z, c6.w
mad.f32 r1.z, c4.y, r0.w, r1.z
mad.f32 r1.w, c5.y, r0.w, r1.w
mad.f32 r2.x, c6.y, r0.w, r2.x
mad.f32 r1.z, c4.z, r1.x, r1.z
mad.f32 r1.w, c5.z, r1.x, r1.w
mad.f32 r2.x, c6.z, r1.x, r2.x
mov.f32f32 r2.y, c7.x
nop
end
