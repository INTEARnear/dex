import { b } from "@zorsh/zorsh";

export const XykPoolNeedsUpgradeArgsSchema = b.struct({
  pool_id: b.u32(),
});
export type XykPoolNeedsUpgradeArgs = b.infer<
  typeof XykPoolNeedsUpgradeArgsSchema
>;
