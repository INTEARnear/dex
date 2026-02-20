import { b } from "@zorsh/zorsh";

export const XykRegisterLiquidityArgsSchema = b.struct({
  pool_id: b.u32(),
});
export type XykRegisterLiquidityArgs = b.infer<
  typeof XykRegisterLiquidityArgsSchema
>;
