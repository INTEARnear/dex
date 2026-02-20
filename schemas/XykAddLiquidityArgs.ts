import { b } from "@zorsh/zorsh";

export const XykAddLiquidityArgsSchema = b.struct({
  pool_id: b.u32(),
  min_shares_received: b.option(NonZeroU128Schema),
});
export type XykAddLiquidityArgs = b.infer<typeof XykAddLiquidityArgsSchema>;
