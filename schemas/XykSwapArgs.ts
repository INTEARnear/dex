import { b } from "@zorsh/zorsh";

export const XykSwapArgsSchema = b.struct({
  pool_id: b.u32(),
});
export type XykSwapArgs = b.infer<typeof XykSwapArgsSchema>;
