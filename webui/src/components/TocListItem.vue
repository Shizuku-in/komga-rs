<template>
  <v-list-item link>
    <div>
      <v-list-item-title
        @click.stop="goto(item)"
        :title="item.title"
        style="width: 100px"
        :class="item.current ? 'text-primary' : ''"
        :style="item.href ? 'cursor: pointer' : undefined"
      >
        <v-badge
          :model-value="!!item.current"
          color="primary"
          dot location="top start"
          offset-y="0"
          offset-x="-5"
        />
        {{ item.title }}
      </v-list-item-title>
    </div>
  </v-list-item>
</template>

<script lang="ts">
import {defineComponent, PropType} from 'vue'
import {TocEntry} from '@/types/epub'

export default defineComponent({
  name: 'TocListItem',
  props: {
    item: {
      type: Object as PropType<TocEntry>,
      required: true,
    },
  },
  methods: {
    goto(element: TocEntry) {
      this.$emit('goto', element)
    },
  },
})
</script>

<style>
</style>
