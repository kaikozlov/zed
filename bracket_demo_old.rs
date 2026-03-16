fn main() {
    // Current Behavior

    // Direct adjacent brackets
    [[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]

    // Mixed delimiters at nearby depths
    let _ = vec![(Some([1, 2, 3]), Ok::<_, ()>({ [4, 5, 6] }))];

    // Multiline nesting
    let _value = foo(
        bar(
            baz([
                alpha({ beta(gamma([delta(), epsilon()])) }),
                zeta([eta({ theta(iota()) })]),
            ]),
        ),
    );

    if true {
        while let Some(value) = maybe_call(vec![Some((1, [2, 3, 4]))]) {
            println!("{value:?}");
        }
    }
}
