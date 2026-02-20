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

export const XykWithdrawFeesArgsSchema = b.struct({
  assets: b.vec(AssetIdSchema),
});
export type XykWithdrawFeesArgs = b.infer<typeof XykWithdrawFeesArgsSchema>;
