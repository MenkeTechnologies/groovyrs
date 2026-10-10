def t = { l, c -> try { println l + ' = ' + c() } catch (e) { println l + ' ! ' + e.getClass().getName() + ': ' + (e.message ?: "").readLines()[0] } }
t('list[5..6]') { [1, 2, 3][5..6] }
t('list[1..5]') { [1, 2, 3][1..5] }
t('list[-5..-1]') { [1, 2, 3][-5..-1] }
t('list[2..0]') { [1, 2, 3][2..0] }
t('list[-1..-3]') { [1, 2, 3][-1..-3] }
t('str[1..5]') { 'abc'[1..5] }
t('str[-5..-1]') { 'ab'[-5..-1] }
t('str[2..0]') { 'x'[2..0] }
t('rrange[1..2]') { (5..1)[1..2] }
t('rrange[2..0]') { (5..1)[2..0] }
t('crrange[1..2]') { ('e'..'a')[1..2] }
t('inject-empty') { [].inject { x, y -> x + y } }
t('r.asList') { (1..3).asList().getClass().getSimpleName() }
t('r.sort') { (1..3).sort { -it } }
t('r.sort.empty') { (1..<1).sort { -it } }
t('dec.isCase') { (1.0..3.0).isCase(3) }
t('step0') { (1..10).step(0) }
t('null<1') { null < 1 }
t('1<null') { 1 < null }
t('list<list') { [1, 2] < [1, 3] }
t('bool<int') { true < 1 }
t('0.0==false') { 0.0 == false }
t('1G==true') { 1G == true }
t('same list <=>') { def l = [1]; l <=> l }
t('diff list <=>') { [1] <=> [1] }
t('plus1') { 1 + [2] }
t('times1') { 2 * [3] }
t('plus2') { 1 + [2, 3] }
t('equals bool') { true.equals(true) }
t('equals obj') { new Object().equals(null) }
t('iter truth') { [].iterator() ? 'T' : 'F' }
t('iter truth2') { [1].iterator() ? 'T' : 'F' }
t('inspect str') { 'it\'s\n'.inspect() }
t('inspect sb') { new StringBuilder('ab').inspect() }
t('class of class') { Integer.getClass().getName() }
t('null idx') { def n = null; n[0] }
t("null set") { null.foo = 1 }
t('strsum') { 'abc'.sum() }
t('str next empty') { ''.next().size() }
t('str+chars') { 'x' + 'abc'.toCharArray() }
t('matcher oob') { ('abc' =~ /l+/)[0] }
t('lines') { 'a\nb'.lines().toList() }
t('sum noStack') { [[1], [2]].sum(0) }
