# intear-dex-prototype-001

this piece of garbage was supposed to be a wasm-in-wasm executor / dex engine that combines multiple dex contracts into one smart contract on near, to allow atomic chaining and parallel routes in intear dex aggregator. unfortunately even this simple dex (not even a dex, just a hello world template) takes more than 100 tgas due to interpreting wasm instead of having it precompiled, we won't be able to go too far with the current 300 tgas limitation. maybe will be increased in the future and this project will be viable again.
