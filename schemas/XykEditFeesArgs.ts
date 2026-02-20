import { b } from "@zorsh/zorsh";

export const AccountIdSchema = b.struct({});
export type AccountId = b.infer<typeof AccountIdSchema>;

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

export const XykEditFeesArgsSchema = b.struct({
  pool_id: b.u32(),
  fees: XykFeeConfigurationSchema,
});
export type XykEditFeesArgs = b.infer<typeof XykEditFeesArgsSchema>;

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
