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

export const XykGetPendingFeesArgsSchema = b.struct({
  account_id: AccountIdSchema,
  asset_ids: b.vec(AssetIdSchema),
});
export type XykGetPendingFeesArgs = b.infer<typeof XykGetPendingFeesArgsSchema>;
