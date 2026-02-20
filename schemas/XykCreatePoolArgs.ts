import { b } from "@zorsh/zorsh";

export const AccountIdSchema = b.struct({});
export type AccountId = b.infer<typeof AccountIdSchema>;

export const AssetIdSchema = b.enum({
  Near: b.unit(),
  Nep141: AccountIdSchema,
  Nep245: AccountIdSchema,
  Nep171: AccountIdSchema,
});
export type AssetId = b.infer<typeof AssetIdSchema>;

export const XykCurrentFeesSchema = b.struct({
  receivers: b.vec(b.tuple([XykFeeReceiverSchema, b.u32()])),
});
export type XykCurrentFees = b.infer<typeof XykCurrentFeesSchema>;

export const XykV1FeeConfigurationSchema = b.struct({
  receivers: b.vec(b.tuple([XykFeeReceiverSchema, XykFeeAmountSchema])),
});
export type XykV1FeeConfiguration = b.infer<typeof XykV1FeeConfigurationSchema>;

export const XykFeeConfigurationSchema = b.enum({
  V1: XykCurrentFeesSchema,
  V2: XykV1FeeConfigurationSchema,
});
export type XykFeeConfiguration = b.infer<typeof XykFeeConfigurationSchema>;

export const XykPoolTypeSchema = b.enum({
  PrivateLatest: b.unit(),
  PublicLatest: b.unit(),
  LaunchLatest: b.struct({
    phantom_liquidity_near: b.u128(),
  }),
  LaunchV1: b.struct({
    phantom_liquidity_near: b.u128(),
  }),
  PrivateV1: b.unit(),
  PublicV1: b.unit(),
  PrivateV2: b.unit(),
  PublicV2: b.unit(),
});
export type XykPoolType = b.infer<typeof XykPoolTypeSchema>;

export const XykCreatePoolArgsSchema = b.struct({
  assets: b.tuple([AssetIdSchema, AssetIdSchema]),
  fees: XykFeeConfigurationSchema,
  pool_type: XykPoolTypeSchema,
});
export type XykCreatePoolArgs = b.infer<typeof XykCreatePoolArgsSchema>;

export const XykScheduledFeeCurveSchema = b.enum({
  Linear: b.unit(),
});
export type XykScheduledFeeCurve = b.infer<typeof XykScheduledFeeCurveSchema>;

export const XykFeeAmountSchema = b.enum({
  Fixed: b.u32(),
  Scheduled: b.struct({
    start: b.tuple([b.u64(), b.u32()]),
    end: b.tuple([b.u64(), b.u32()]),
    curve: XykScheduledFeeCurveSchema,
  }),
  Dynamic: b.struct({
    min: b.u32(),
    max: b.u32(),
  }),
});
export type XykFeeAmount = b.infer<typeof XykFeeAmountSchema>;

export const XykFeeReceiverSchema = b.enum({
  Account: AccountIdSchema,
  Pool: b.unit(),
});
export type XykFeeReceiver = b.infer<typeof XykFeeReceiverSchema>;
