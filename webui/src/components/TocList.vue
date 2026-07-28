<template>
  <v-list v-if="toc">
    <template v-for="(t, i) in toc" :key="i">
      <v-list-group v-if="t.children" :value="i">
        <template #activator="{props}">
          <toc-list-item v-bind="props" :item="t" @goto="goto" class="ps-0"/>
        </template>

        <template v-for="(child, childIndex) in t.children" :key="childIndex">
          <toc-list-item :item="child"
                         @goto="goto"
                         :class="`ms-${(child.level - 1) * 4}`"
          />
        </template>
      </v-list-group>

      <!--  Single item    -->
      <template v-else>
        <toc-list-item :item="t" @goto="goto"/>
      </template>
    </template>
  </v-list>
</template>

<script lang="ts">
import {defineComponent, PropType} from 'vue'
import {TocEntry} from '@/types/epub'
import TocListItem from '@/components/TocListItem.vue'

export default defineComponent({
  name: 'TocList',
  components: {TocListItem},
  data: () => ({}),
  props: {
    toc: {
      type: Array as PropType<TocEntry>[],
      required: false,
    },
  },
  computed: {},
  methods: {
    goto(element: TocEntry) {
      this.$emit('goto', element)
    },
  },
})
</script>

<style>
</style>
