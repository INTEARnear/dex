import { b } from "@zorsh/zorsh";

export const XykRemoveLiquidityArgsSchema = b.struct({
  pool_id: b.u32(),
  shares_to_remove: b.option(NonZeroU128Schema),
  min_assets_received: b.option(b.tuple([b.u128(), b.u128()])),
});
export type XykRemoveLiquidityArgs = b.infer<
  typeof XykRemoveLiquidityArgsSchema
>;
