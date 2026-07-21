for (i in 1..3) {
    for (j in 1..3) {
        if (j == 2) continue
        println i + "x" + j + "=" + (i * j)
    }
}
